package com.samsung.e830

import android.net.Uri
import android.Manifest
import android.bluetooth.BluetoothAdapter
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.provider.OpenableColumns
import android.util.Log
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.widget.GridLayout
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.activity.addCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.android.material.appbar.MaterialToolbar
import com.google.android.material.button.MaterialButton
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.UUID

class EmulatorActivity : AppCompatActivity() {
    private val sessionHandle = AtomicLong(0)
    private val generation = AtomicLong(0)
    private val pollPending = AtomicBoolean(false)
    // Start, polling, input and destruction are serialized off the UI thread.
    private val nativeExecutor = Executors.newSingleThreadExecutor { task ->
        Thread(task, "phone-android-native").apply { isDaemon = true }
    }
    private val fileExecutor = Executors.newSingleThreadExecutor { task ->
        Thread(task, "ph0nix-file-io").apply { isDaemon = true }
    }
    private lateinit var root: View
    private lateinit var toolbar: MaterialToolbar
    private lateinit var lcd: LcdView
    private lateinit var waiting: TextView
    private lateinit var status: TextView
    private lateinit var actions: View
    private lateinit var fullscreenButton: MaterialButton
    private lateinit var preferences: AppPreferences
    @Volatile private var audioPlayer: AudioPlayer? = null
    private val heldTouch = mutableSetOf<Int>()
    private val heldPhysical = mutableSetOf<Int>()
    private var sentMask = 0
    private var foreground = false
    private var sessionStarting = false
    private var sessionStopped = false
    private var stopPending = false
    private var leaving = false
    private var leaveStartedAt = 0L
    private var lastDiagnostics: JSONObject? = null
    private var fullscreen = false
    private var bindings = emptyList<Binding>()
    private var developerPanel: DeveloperPanel? = null
    private var diagnosticsDueNs = 0L
    private var recordedPresentations = 0L
    @Volatile private var audioProblem: String? = null
    private var latestFiles = JSONObject()
    private val pendingFiles = ArrayDeque<PendingFile>()
    private var activeFile: PendingFile? = null
    private var activeFileId = 0L
    private var lastFileErrorId = 0L
    private var documentExport: File? = null
    private var cardBusy = false
    private lateinit var bluetooth: BluetoothBridge
    private var bluetoothStatus: TextView? = null
    private var bluetoothMessage: String? = null
    private val receivedBluetooth = mutableMapOf<String, JSONObject>()
    private var afterBluetoothPermission: (() -> Unit)? = null
    private val bluetoothPermission = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { result ->
        val action = afterBluetoothPermission
        afterBluetoothPermission = null
        if (result.values.all { it }) action?.invoke()
        else Toast.makeText(this, R.string.bluetooth_permission, Toast.LENGTH_LONG).show()
    }
    private val discoverable = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        if (it.resultCode > 0) runCatching { bluetooth.receive() }.onFailure { showBluetoothMessage(it.message.orEmpty()) }
    }
    private val importBluetoothDocument = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) importBluetoothFile(uri)
    }
    private val importCardDocument = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) fileExecutor.execute {
            val directory = MemoryStickImages.directory(filesDir)
            val id = UUID.randomUUID()
            val image = File(directory, "imported-$id.img")
            val staged = File(directory, ".$id.tmp")
            val result = runCatching {
                check(directory.isDirectory || directory.mkdirs()) { getString(R.string.card_folder_failed) }
                try {
                    val input = contentResolver.openInputStream(uri) ?: error(getString(R.string.card_open_failed))
                    input.use { staged.outputStream().use { output -> copyLimited(input, output, 1024L * 1024L * 1024L) } }
                    val request = JSONObject().put("image", staged.absolutePath).put("operation", "info")
                    val response = JSONObject(NativeBridge.nativeCardRequest(request.toString()))
                    check(!response.has("error")) { response.optString("error") }
                    check(staged.renameTo(image)) { getString(R.string.card_save_failed) }
                } finally {
                    staged.delete()
                }
            }
            if (result.isSuccess) notifyMemoryStickChanged(this, image.name)
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                result.onSuccess { Toast.makeText(this, R.string.card_imported, Toast.LENGTH_SHORT).show() }
                    .onFailure { Toast.makeText(this, getString(R.string.card_import_failed, it.localizedMessage ?: getString(R.string.unknown_error)), Toast.LENGTH_LONG).show() }
            }
        }
    }
    private val exportDocument = registerForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        val image = documentExport
        documentExport = null
        if (uri != null) fileExecutor.execute {
            val result = runCatching {
                writeDocumentExport(uri, image)
            }
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                val message = result.fold({ getString(R.string.card_exported) }, { getString(R.string.export_failed, it.localizedMessage ?: getString(R.string.unknown_error)) })
                Toast.makeText(this, message, Toast.LENGTH_LONG).show()
            }
        }
    }
    private val selection by lazy { intent.getStringExtra(EXTRA_SELECTION).orEmpty() }
    private val phoneId by lazy { intent.getStringExtra(EXTRA_PHONE_ID)?.takeIf { StorageDocument.validId(it) } }
    private val bootCard by lazy { intent.getStringExtra(EXTRA_CARD_IMAGE).orEmpty() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        preferences = AppPreferences(this)
        documentExport = savedInstanceState?.getString("document_export")?.let(::File)
        bluetooth = BluetoothBridge(this, File(filesDir, "bluetooth-inbox"), { level, message ->
            val handle = sessionHandle.get(); val token = generation.get()
            if (handle != 0L) runCatching { nativeExecutor.execute {
                if (sessionHandle.get() == handle && generation.get() == token)
                    NativeBridge.nativeLog(handle, level, "bluetooth", message)
            } }
        }, { message -> runOnUiThread { showBluetoothMessage(message) } }, { file ->
            runCatching { File(file.getString("path") + ".json").writeText(file.toString()) }
                .onFailure { android.util.Log.e("ph0nix", "Could not save Bluetooth receipt", it) }
            runOnUiThread {
                receivedBluetooth[file.getString("path")] = file
                if (!isFinishing && !isDestroyed) {
                    val destination = listOf("Phone memory", "Other")
                    MaterialAlertDialogBuilder(this).setTitle(getString(R.string.bluetooth_import_title, file.optString("name")))
                        .setMessage(getString(R.string.bluetooth_import_description, file.optLong("bytes"), destination.joinToString("/")))
                        .setNegativeButton(R.string.bluetooth_keep_inbox, null).setPositiveButton(R.string.import_file) { _, _ ->
                            enqueueFile(JSONObject().put("operation", "import").put("path", JSONArray(destination + file.getString("name")))
                                .put("host", file.getString("path")).put("replace", false).put("move_source", true)) {
                                File(file.getString("path") + ".json").delete()
                            }
                        }.show()
                }
            }
        })
        WindowCompat.setDecorFitsSystemWindows(window, false)
        window.attributes = window.attributes.apply { preferredRefreshRate = 60f }
        setContentView(R.layout.activity_emulator)
        root = findViewById(R.id.root)
        toolbar = findViewById(R.id.toolbar)
        lcd = findViewById(R.id.lcd_view)
        waiting = findViewById(R.id.waiting)
        status = findViewById(R.id.status)
        actions = findViewById(R.id.actions)
        fullscreenButton = findViewById(R.id.btn_fullscreen)
        val device = runCatching { JSONObject(selection).getString("device") }.getOrDefault("k750i")
        toolbar.title = runCatching { DeviceProfiles.get(device).name }.getOrDefault(getString(R.string.firmware_title))
        toolbar.menu.add(0, MENU_BLUETOOTH, 1, R.string.bluetooth_files)
        toolbar.menu.add(0, MENU_CARDS, 2, R.string.card_images)
        toolbar.menu.add(0, MENU_DEVELOPER, 3, R.string.diagnostics_menu)
        toolbar.setOnMenuItemClickListener { item ->
            when (item.itemId) {
                MENU_BLUETOOTH -> showBluetooth()
                MENU_CARDS -> showCards()
                MENU_DEVELOPER -> openDiagnostics()
                else -> return@setOnMenuItemClickListener false
            }
            true
        }
        val keys = JSONArray(NativeBridge.nativeKeyBindings(device))
        bindings = List(keys.length()) { index ->
            keys.getJSONObject(index).let { Binding(it.getString("label"), it.getInt("mask")) }
        }
        buildKeypad()
        lcd.showFps = preferences.showFps
        applySystemInsets()
        fullscreenButton.setOnClickListener { setFullscreen(!fullscreen) }
        findViewById<View>(R.id.btn_back).setOnClickListener { leaveEmulator() }
        onBackPressedDispatcher.addCallback(this) { leaveEmulator() }
        lcd.setOnClickListener { if (fullscreen) setFullscreen(false) }
        setFullscreen(savedInstanceState?.getBoolean("fullscreen") ?: preferences.fullscreen)
        if (selection.isBlank()) finish()
    }

    override fun onResume() {
        super.onResume()
        foreground = true
        root.keepScreenOn = preferences.keepAwake
        if (selection.isBlank() || sessionStopped) return
        if (sessionStarting || sessionHandle.get() != 0L) {
            setSessionPaused(false)
            startAudio(generation.get())
            lcd.setRunning(lcd.onPresentationFrame != null)
        } else {
            startSession()
        }
    }

    override fun onPause() {
        releaseKeys()
        foreground = false
        root.keepScreenOn = false
        lcd.setRunning(false)
        audioPlayer?.close()
        audioPlayer = null
        if (sessionStarting || sessionHandle.get() != 0L) setSessionPaused(true)
        super.onPause()
    }

    private fun setSessionPaused(paused: Boolean) {
        val token = generation.get()
        // Resolve the handle on the executor, including when starting is still in flight.
        nativeExecutor.execute {
            val handle = sessionHandle.get()
            if (generation.get() == token && handle != 0L) NativeBridge.nativeSetPaused(handle, paused)
        }
    }

    override fun onDestroy() {
        stopSession()
        developerPanel?.dismiss()
        bluetooth.close()
        nativeExecutor.shutdown() // Let queued checkpoint saves finish.
        fileExecutor.shutdown()
        lcd.releaseFrame()
        super.onDestroy()
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean("fullscreen", fullscreen)
        outState.putString("document_export", documentExport?.absolutePath)
        super.onSaveInstanceState(outState)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (!hasFocus) releaseKeys()
        if (hasFocus && fullscreen) setFullscreen(true)
    }

    private fun applySystemInsets() {
        ViewCompat.setOnApplyWindowInsetsListener(root) { target, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
            target.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }
        WindowInsetsControllerCompat(window, root).apply {
            isAppearanceLightStatusBars = false
            isAppearanceLightNavigationBars = false
        }
        ViewCompat.requestApplyInsets(root)
    }

    private fun startAudio(token: Long) {
        if (audioPlayer != null || !preferences.sound) return
        audioProblem = null
        audioPlayer = AudioPlayer { error ->
            if (generation.get() == token) {
                audioProblem = error
                val active = sessionHandle.get()
                if (active != 0L) runCatching { nativeExecutor.execute {
                    if (generation.get() == token && sessionHandle.get() == active)
                        NativeBridge.nativeLog(active, "error", "android-audio", error)
                } }
            }
        }
    }

    private fun startSession() {
        val token = generation.incrementAndGet()
        sessionStarting = true
        startAudio(token)
        status.setText(R.string.emulator_starting)
        waiting.visibility = View.VISIBLE
        nativeExecutor.execute {
            val handle = try {
                NativeBridge.nativeConfigure(
                    File(applicationInfo.nativeLibraryDir, "libdb2010_engine.so").absolutePath,
                    File(filesDir, "sessions").absolutePath, preferences.resume,
                )
                NativeBridge.nativeStartSelection(selection, intent.getStringExtra(EXTRA_MEMORY_WORKSPACE).orEmpty(), bootCard).also {
                    check(it != 0L) { getString(R.string.phone_start_failed) }
                }
            } catch (error: Throwable) {
                runOnUiThread {
                    if (generation.get() == token) {
                        sessionStarting = false
                        audioPlayer?.close()
                        audioPlayer = null
                        waiting.visibility = View.GONE
                        status.text = error.message ?: getString(R.string.phone_start_failed)
                    }
                }
                return@execute
            }
            sessionHandle.set(handle)
            notifyInternalStorageChanged(this, phoneId)
            if (bootCard.isNotBlank()) notifyMemoryStickChanged(this, File(bootCard).name)
            // A stop requested during startup is queued behind this task.
            if (generation.get() != token) return@execute
            diagnosticsDueNs = 0L
            audioProblem?.let { NativeBridge.nativeLog(handle, "error", "android-audio", it) }
            runOnUiThread {
                if (generation.get() == token && sessionHandle.get() == handle) {
                    sessionStarting = false
                    recordedPresentations = lcd.presentationCount
                    latestFiles = JSONObject()
                    lcd.onPresentationFrame = { pollNative(token, handle) }
                    lcd.setRunning(foreground)
                }
            }
        }
    }

    private fun pollNative(token: Long, handle: Long) {
        if (!pollPending.compareAndSet(false, true)) return
        val presented = lcd.presentationCount
        val renderNanos = lcd.lastRenderNanos
        nativeExecutor.execute {
            try {
                if (generation.get() != token || sessionHandle.get() != handle) return@execute
                val player = audioPlayer
                val newlyPresented = (presented - recordedPresentations).coerceIn(0L, 240L).toInt()
                recordedPresentations = presented
                NativeBridge.nativeRecordPresentations(handle, newlyPresented)
                if (newlyPresented > 0) NativeBridge.nativeRecordRender(handle, renderNanos)
                val message = NativeBridge.nativePollStatus(handle)
                val frame = NativeBridge.nativeTakeFrame(handle)
                val running = NativeBridge.nativeIsRunning(handle)
                for (index in 0 until 10) {
                    val packet = NativeBridge.nativeTakeAudio(handle) ?: break
                    player?.enqueue(packet)
                }
                val now = System.nanoTime()
                val sampleServices = now >= diagnosticsDueNs || !running
                val diagnostic = if (sampleServices) {
                    diagnosticsDueNs = now + 250_000_000L
                    JSONObject(NativeBridge.nativeDiagnostics(handle))
                } else null
                val files = if (sampleServices) JSONObject(NativeBridge.nativeFiles(handle)) else null
                runOnUiThread {
                    if (generation.get() == token && sessionHandle.get() == handle) {
                        frame?.let { lcd.setFrame(it); waiting.visibility = View.GONE }
                        val statusText = when {
                            !running -> message
                            waiting.visibility == View.VISIBLE -> getString(R.string.emulator_starting)
                            diagnostic != null -> getString(
                                R.string.emulator_running, toolbar.title, diagnostic.optLong("guest_virtual_ns") / 1e9,
                            )
                            else -> null
                        }
                        if (statusText != null && status.text.toString() != statusText) status.text = statusText
                        diagnostic?.let { showDiagnostics(it, token, handle) }
                        files?.let { updateFiles(it) }
                        if (!running) stopSession()
                    }
                }
            } catch (error: Throwable) {
                runCatching { NativeBridge.nativeLog(handle, "error", "android", error.message ?: error.toString()) }
                val diagnostic = runCatching { JSONObject(NativeBridge.nativeDiagnostics(handle)) }.getOrNull()
                runOnUiThread {
                    if (generation.get() == token) {
                        diagnostic?.let { showDiagnostics(it, token, handle) }
                        status.text = error.message ?: getString(R.string.phone_stopped_unexpectedly)
                        stopSession()
                    }
                }
            } finally { pollPending.set(false) }
        }
    }

    private fun leaveEmulator() {
        if (leaving) return
        leaveStartedAt = SystemClock.elapsedRealtime()
        if (sessionStopped && !stopPending) {
            finishToLibrary()
            return
        }
        leaving = true
        if (fullscreen) setFullscreen(false)
        status.text = if (bootCard.isBlank()) getString(R.string.phone_saving_memory)
            else getString(R.string.phone_saving_card)
        stopSession()
    }

    private fun stopSession() {
        if (sessionStopped) return
        sessionStopped = true
        stopPending = true
        releaseKeys()
        val token = generation.incrementAndGet()
        sessionStarting = false
        lcd.setRunning(false)
        lcd.onPresentationFrame = null
        waiting.visibility = View.GONE
        activeFile = null; activeFileId = 0L
        audioPlayer?.close()
        audioPlayer = null
        nativeExecutor.execute {
            val handle = sessionHandle.getAndSet(0)
            val (error, report) = if (handle != 0L) stopAndDestroy(handle) else Pair(null, null)
            notifyInternalStorageChanged(this, phoneId)
            if (handle != 0L && bootCard.isNotBlank()) notifyMemoryStickChanged(this, File(bootCard).name)
            val notificationsCompletedAt = SystemClock.elapsedRealtime()
            runOnUiThread {
                stopPending = false
                if (isFinishing || isDestroyed) return@runOnUiThread
                report?.let { showDiagnostics(it, token, 0) }
                if (error != null) {
                    if (fullscreen) setFullscreen(false)
                    status.text = error
                    if (leaving) {
                        MaterialAlertDialogBuilder(this)
                            .setTitle(R.string.phone_stopped_error)
                            .setMessage(error)
                            .setPositiveButton(android.R.string.ok, null)
                            .show()
                    }
                    leaving = false
                } else if (leaving) {
                    if (handle != 0L) {
                        if (notificationsCompletedAt >= leaveStartedAt) {
                            Log.i(SAVE_TIMING_TAG, "storage_notifications_complete elapsed_ms=${notificationsCompletedAt - leaveStartedAt}")
                        } else {
                            Log.i(SAVE_TIMING_TAG, "storage_notifications_already_complete")
                        }
                    }
                    finishToLibrary()
                }
            }
        }
    }

    private fun finishToLibrary() {
        setResult(RESULT_OK, Intent().putExtra(EXTRA_LEAVE_STARTED_AT, leaveStartedAt))
        finish()
    }

    private fun openDiagnostics() {
        releaseKeys()
        val token = generation.get()
        val handle = sessionHandle.get()
        if (sessionStopped || handle == 0L) {
            lastDiagnostics?.let { showDiagnostics(it, token, 0, open = true) }
            return
        }
        nativeExecutor.execute {
            if (generation.get() == token && sessionHandle.get() == handle) {
                NativeBridge.nativeSetDeveloperMode(handle, true)
                val snapshot = JSONObject(NativeBridge.nativeDiagnostics(handle))
                runOnUiThread {
                    if (generation.get() == token && sessionHandle.get() == handle && !isFinishing)
                        showDiagnostics(snapshot, token, handle)
                }
            }
        }
    }

    private fun showDiagnostics(snapshot: JSONObject, token: Long, handle: Long, open: Boolean = false) {
        lastDiagnostics = snapshot
        if (developerPanel?.isShowing == true) {
            developerPanel?.update(snapshot)
        } else if (open || (handle != 0L && snapshot.optBoolean("developer_mode"))) {
            releaseKeys()
            developerPanel = DeveloperPanel(this) {
                if (handle != 0L && generation.get() == token && sessionHandle.get() == handle) nativeExecutor.execute {
                    if (generation.get() == token && sessionHandle.get() == handle)
                        NativeBridge.nativeSetDeveloperMode(handle, false)
                }
            }.also { it.show(snapshot) }
        }
    }

    private fun enqueueFile(request: JSONObject, completed: ((JSONObject) -> Unit)? = null): Boolean {
        if (isFinishing || isDestroyed) return false
        if (pendingFiles.size >= 8) {
            showBluetoothMessage(getString(R.string.bluetooth_queue_full))
            return false
        }
        pendingFiles.addLast(PendingFile(request, completed))
        drainFiles()
        return true
    }

    private fun drainFiles() {
        if (!foreground || sessionStopped || activeFile != null || pendingFiles.isEmpty() ||
            !latestFiles.optBoolean("attached") || latestFiles.optBoolean("busy")) return
        val handle = sessionHandle.get(); val token = generation.get()
        if (handle == 0L) return
        val action = pendingFiles.removeFirst()
        activeFile = action
        nativeExecutor.execute {
            if (sessionHandle.get() != handle || generation.get() != token) {
                runOnUiThread { if (activeFile === action) { activeFile = null; pendingFiles.addFirst(action) } }
                return@execute
            }
            val result = runCatching {
                val response = JSONObject(NativeBridge.nativeFileRequest(handle, action.request.toString()))
                check(!response.has("error")) { response.optString("error") }
                response.getLong("id")
            }
            runOnUiThread {
                if (activeFile !== action) return@runOnUiThread
                result.onSuccess { activeFileId = it }.onFailure { error ->
                    activeFile = null
                    showBluetoothMessage(error.message.orEmpty())
                }
            }
        }
    }

    private fun updateFiles(snapshot: JSONObject) {
        latestFiles = snapshot
        val job = snapshot.optJSONObject("job")
        if (job != null && activeFileId != 0L && job.optLong("id") == activeFileId && !snapshot.optBoolean("busy")) {
            val action = activeFile
            activeFile = null; activeFileId = 0L
            val succeeded = job.optString("phase") == "complete" && job.isNull("error")
            if (succeeded) {
                action?.completed?.invoke(job)
                showBluetoothMessage(getString(R.string.bluetooth_saved))
            } else {
                showBluetoothMessage(job.optString("error").takeUnless { job.isNull("error") || it.isBlank() }
                    ?: getString(R.string.bluetooth_interrupted))
            }
        }
        if (job != null && !job.isNull("error") && job.optLong("id") != lastFileErrorId) {
            lastFileErrorId = job.optLong("id")
            val handle = sessionHandle.get(); val token = generation.get(); val error = job.optString("error")
            nativeExecutor.execute {
                if (handle != 0L && generation.get() == token && sessionHandle.get() == handle)
                    NativeBridge.nativeLog(handle, "error", "phone-files", error)
            }
        }
        drainFiles()
    }

    private fun importBluetoothFile(uri: Uri) {
        fileExecutor.execute {
            val destination = listOf("Phone memory", "Other")
            var staging: File? = null
            var name = "imported-file"
            val result = runCatching {
                val displayName = contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
                    if (cursor.moveToFirst()) cursor.getString(0) else null
                } ?: name
                name = displayName.replace('/', '_').replace('\\', '_').replace('\u0000', '_').take(180)
                    .takeUnless { it.isBlank() || it == "." || it == ".." } ?: "imported-file"
                val target = File(filesDir, "transfer-staging/${UUID.randomUUID()}/$name")
                staging = target; target.parentFile!!.mkdirs()
                contentResolver.openInputStream(uri)!!.use { input -> target.outputStream().use { copyLimited(input, it, 32L * 1024L * 1024L) } }
            }
            runOnUiThread {
                if (isFinishing || isDestroyed) { staging?.delete(); return@runOnUiThread }
                result.onSuccess {
                    val accepted = enqueueFile(JSONObject().put("operation", "import").put("path", JSONArray(destination + name))
                        .put("host", staging!!.absolutePath).put("replace", false).put("move_source", true))
                    if (!accepted) staging?.delete()
                }.onFailure { staging?.delete(); showBluetoothMessage(getString(R.string.import_failed, it.localizedMessage ?: getString(R.string.unknown_error))) }
            }
        }
    }

    private fun showCards() {
        val cards = MemoryStickImages.list(MemoryStickImages.directory(filesDir))
        val labels = listOf(R.string.card_create, R.string.card_import).map(::getString) + cards.map {
            val label = MemoryStickImages.label(this, it)
            if (it.absolutePath == bootCard) getString(R.string.card_inserted_label, label) else label
        }
        MaterialAlertDialogBuilder(this).setTitle(R.string.card_removable_images)
            .setItems(labels.toTypedArray()) { _, index ->
                when (index) {
                    0 -> createCard()
                    1 -> importCardDocument.launch(arrayOf("*/*"))
                    else -> cardActions(cards[index - 2])
                }
            }.setNegativeButton(R.string.close, null).show()
    }

    private fun createCard() {
        val layout = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(20), 0, dp(20), 0) }
        val name = android.widget.EditText(this).apply { hint = getString(R.string.card_image_name); setText("ph0nix-card"); isSingleLine = true }
        val size = android.widget.EditText(this).apply { hint = getString(R.string.card_image_size); setText("64"); inputType = android.text.InputType.TYPE_CLASS_NUMBER }
        layout.addView(name); layout.addView(size)
        MaterialAlertDialogBuilder(this).setTitle(R.string.card_create_fat16).setView(layout)
            .setMessage(R.string.card_choose_at_start)
            .setNegativeButton(R.string.cancel, null).setPositiveButton(R.string.create) { _, _ ->
                val label = name.text.toString().trim()
                val mib = size.text.toString().toLongOrNull()
                if (!StorageDocument.validComponent(label) || !MemoryStickImages.validName("$label.img") ||
                    mib == null || mib !in 16L..1024L) {
                    Toast.makeText(this, R.string.card_invalid_name_or_size, Toast.LENGTH_LONG).show()
                    return@setPositiveButton
                }
                val image = File(MemoryStickImages.directory(filesDir), "$label.img")
                createCardImage(image, mib)
            }.show()
    }

    private fun cardActions(image: File) {
        if (image.absolutePath == bootCard) {
            MaterialAlertDialogBuilder(this).setTitle(image.name)
                .setMessage(R.string.card_inserted_description)
                .setPositiveButton(R.string.close, null).show()
            return
        }
        MaterialAlertDialogBuilder(this).setTitle(image.name)
            .setMessage(getString(R.string.card_fat16_description, image.length() / 1048576, getString(R.string.card_choose_at_start)))
            .setNeutralButton(R.string.card_export) { _, _ ->
                if (image.isFile && image.length() > 0) {
                    documentExport = image
                    exportDocument.launch(image.name)
                } else {
                    Toast.makeText(this, R.string.card_missing_or_empty, Toast.LENGTH_LONG).show()
                }
            }.setNegativeButton(R.string.close, null).show()
    }

    private fun createCardImage(image: File, mib: Long) {
        if (cardBusy) {
            Toast.makeText(this, R.string.card_busy, Toast.LENGTH_SHORT).show()
            return
        }
        cardBusy = true
        fileExecutor.execute {
            val result = runCatching {
                val request = JSONObject().put("image", image.absolutePath).put("operation", "create").put("size_mib", mib)
                val response = JSONObject(NativeBridge.nativeCardRequest(request.toString()))
                check(!response.has("error")) { response.optString("error") }
                notifyMemoryStickChanged(this, image.name)
            }
            runOnUiThread {
                cardBusy = false
                if (isFinishing || isDestroyed) return@runOnUiThread
                val message = result.fold({ getString(R.string.card_created) }, { getString(R.string.card_create_failed, it.localizedMessage ?: getString(R.string.unknown_error)) })
                Toast.makeText(this, message, Toast.LENGTH_LONG).show()
            }
        }
    }

    private fun copyLimited(input: InputStream, output: OutputStream, maximum: Long) {
        val buffer = ByteArray(32768)
        var total = 0L
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            total += count
            check(total <= maximum) { getString(R.string.file_exceeds_limit, maximum / 1048576) }
            output.write(buffer, 0, count)
        }
    }

    private fun showBluetoothMessage(message: String) {
        bluetoothMessage = message
        bluetoothStatus?.text = message
    }

    private fun showBluetooth() {
        val text = TextView(this).apply { this.text = bluetoothMessage ?: getString(R.string.bluetooth_description); setPadding(dp(20), dp(16), dp(20), dp(16)) }
        bluetoothStatus = text
        val layout = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; addView(text) }
        listOf(R.string.bluetooth_receive, R.string.bluetooth_inbox, R.string.bluetooth_import_received, R.string.bluetooth_cancel).forEachIndexed { choice, label ->
            layout.addView(MaterialButton(this, null, com.google.android.material.R.attr.materialButtonOutlinedStyle).apply {
                setText(label); isAllCaps = false
                setOnClickListener { when (choice) {
                    0 -> withBluetoothPermission(advertise = true) {
                        discoverable.launch(Intent(BluetoothAdapter.ACTION_REQUEST_DISCOVERABLE)
                            .putExtra(BluetoothAdapter.EXTRA_DISCOVERABLE_DURATION, 120))
                    }
                    1 -> showBluetoothInbox()
                    2 -> importBluetoothDocument.launch(arrayOf("*/*"))
                    3 -> bluetooth.cancel()
                } }
            })
        }
        MaterialAlertDialogBuilder(this).setTitle(R.string.bluetooth_files).setView(layout)
            .setPositiveButton(R.string.close, null).show()
    }

    private fun showBluetoothInbox() {
        val savedRecords = File(filesDir, "bluetooth-inbox").listFiles()?.filter { it.isFile && it.name.endsWith(".json") }
            ?.mapNotNull { file -> runCatching { JSONObject(file.readText()) }.getOrNull() }
            .orEmpty()
        val records = (savedRecords + receivedBluetooth.values).distinctBy { it.optString("path") }
            .filter { File(it.optString("path")).isFile }
        if (records.isEmpty()) { Toast.makeText(this, R.string.bluetooth_inbox_empty, Toast.LENGTH_SHORT).show(); return }
        MaterialAlertDialogBuilder(this).setTitle(R.string.bluetooth_inbox).setItems(records.map { it.optString("name") }.toTypedArray()) { _, index ->
            val record = records[index]
            val destination = listOf("Phone memory", "Other")
            enqueueFile(JSONObject().put("operation", "import").put("path", JSONArray(destination + record.getString("name")))
                .put("host", record.getString("path")).put("replace", false).put("move_source", true)) {
                File(record.getString("path") + ".json").delete()
            }
        }.setNegativeButton(R.string.close, null).show()
    }

    private fun withBluetoothPermission(advertise: Boolean = false, action: () -> Unit) {
        val permissions = if (Build.VERSION.SDK_INT >= 31) buildList {
            add(Manifest.permission.BLUETOOTH_CONNECT)
            if (advertise) add(Manifest.permission.BLUETOOTH_ADVERTISE)
        } else emptyList()
        if (permissions.all { checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }) action()
        else { afterBluetoothPermission = action; bluetoothPermission.launch(permissions.toTypedArray()) }
    }

    private data class PendingFile(
        val request: JSONObject,
        val completed: ((JSONObject) -> Unit)?,
    )

    private fun stopAndDestroy(handle: Long): Pair<String?, JSONObject?> {
        val error = try {
            NativeBridge.nativeSetKeys(handle, 0)
            NativeBridge.nativeStop(handle).ifBlank { null }
        } catch (error: Throwable) {
            android.util.Log.e("ph0nix", "Stopping the phone failed", error)
            val message = error.message ?: error.toString()
            runCatching { NativeBridge.nativeLog(handle, "error", "android", message) }
            message
        }
        // Stop joins the worker, including any memory save. Capture the final report first.
        val diagnostic = runCatching { JSONObject(NativeBridge.nativeDiagnostics(handle)) }.getOrNull()
        diagnostic?.optJSONArray("logs")?.let { logs ->
            for (index in 0 until logs.length()) {
                val entry = logs.optJSONObject(index) ?: continue
                if (entry.optString("component") == "save-timing") {
                    Log.i(SAVE_TIMING_TAG, entry.optString("message"))
                }
            }
        }
        NativeBridge.nativeDestroy(handle)
        return Pair(error, diagnostic)
    }

    private fun buildKeypad() {
        val container = findViewById<LinearLayout>(R.id.keypad)
        if (bindings.isEmpty()) { container.visibility = View.GONE; return }
        val grid = GridLayout(this).apply { columnCount = 3 }
        bindings.take(21).forEachIndexed { index, binding ->
            grid.addView(keyButton(binding), GridLayout.LayoutParams().apply {
                width = 0; height = dp(36)
                columnSpec = GridLayout.spec(index % 3, 1f)
                rowSpec = GridLayout.spec(index / 3)
            })
        }
        container.addView(grid)
        if (bindings.size > 21) {
            val row = LinearLayout(this)
            bindings.drop(21).forEach { row.addView(keyButton(it), LinearLayout.LayoutParams(dp(86), dp(36))) }
            container.addView(HorizontalScrollView(this).apply { addView(row) })
        }
    }

    private fun keyButton(binding: Binding) = MaterialButton(
        this, null, com.google.android.material.R.attr.materialButtonOutlinedStyle,
    ).apply {
        text = binding.label; textSize = 12f; isAllCaps = false
        insetTop = 1; insetBottom = 1
        minHeight = 0; minimumHeight = 0; minWidth = 0; minimumWidth = 0
        setPadding(4, 0, 4, 0)
        contentDescription = getString(R.string.key_description, binding.label)
        setOnTouchListener { view, event ->
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    heldTouch.add(binding.mask); view.isPressed = true
                    sendKeys()
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                    heldTouch.remove(binding.mask); view.isPressed = false; sendKeys()
                    if (event.actionMasked == MotionEvent.ACTION_UP) view.performClick()
                }
            }
            true
        }
    }

    private fun releaseKeys() { heldTouch.clear(); heldPhysical.clear(); sendKeys() }
    private fun sendKeys() {
        val mask = if (foreground) (heldTouch + heldPhysical).fold(0) { a, b -> a or b } else 0
        if (mask == sentMask) return
        sentMask = mask
        val token = generation.get(); val handle = sessionHandle.get()
        if (handle != 0L) nativeExecutor.execute {
            if (generation.get() == token && sessionHandle.get() == handle) NativeBridge.nativeSetKeys(handle, mask)
        }
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val label = when (event.keyCode) {
            KeyEvent.KEYCODE_DPAD_UP -> "Up"
            KeyEvent.KEYCODE_DPAD_DOWN -> "Down"
            KeyEvent.KEYCODE_DPAD_LEFT -> "Left"
            KeyEvent.KEYCODE_DPAD_RIGHT -> "Right"
            KeyEvent.KEYCODE_DPAD_CENTER, KeyEvent.KEYCODE_ENTER -> "OK"
            KeyEvent.KEYCODE_F1 -> "Left soft"
            KeyEvent.KEYCODE_F2 -> "Right soft"
            KeyEvent.KEYCODE_DEL -> "C"
            KeyEvent.KEYCODE_STAR -> "*"
            KeyEvent.KEYCODE_POUND -> "#"
            KeyEvent.KEYCODE_VOLUME_UP -> "Volume +"
            KeyEvent.KEYCODE_VOLUME_DOWN -> "Volume −"
            in KeyEvent.KEYCODE_0..KeyEvent.KEYCODE_9 -> (event.keyCode - KeyEvent.KEYCODE_0).toString()
            else -> null
        }
        val binding = bindings.firstOrNull { it.label == label || (label?.length == 1 && it.label.startsWith("$label ")) }
        if (binding == null || !foreground || sessionHandle.get() == 0L) return super.dispatchKeyEvent(event)
        if (event.action == KeyEvent.ACTION_DOWN) heldPhysical.add(binding.mask)
        else if (event.action == KeyEvent.ACTION_UP) heldPhysical.remove(binding.mask)
        sendKeys()
        return true
    }

    private fun setFullscreen(enabled: Boolean) {
        fullscreen = enabled
        preferences.fullscreen = enabled
        toolbar.visibility = if (enabled) View.GONE else View.VISIBLE
        status.visibility = if (enabled) View.GONE else View.VISIBLE
        actions.visibility = if (enabled) View.GONE else View.VISIBLE
        fullscreenButton.setText(if (enabled) R.string.exit_fullscreen else R.string.fullscreen)
        fullscreenButton.setIconResource(if (enabled) R.drawable.ic_fullscreen_exit else R.drawable.ic_fullscreen)
        WindowInsetsControllerCompat(window, root).apply {
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            if (enabled) hide(WindowInsetsCompat.Type.systemBars()) else show(WindowInsetsCompat.Type.systemBars())
        }
        ViewCompat.requestApplyInsets(root)
    }

    private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()
    private data class Binding(val label: String, val mask: Int)
    companion object {
        const val EXTRA_PHONE_ID = "com.samsung.e830.PHONE_ID"
        const val EXTRA_SELECTION = "com.samsung.e830.SELECTION"
        const val EXTRA_MEMORY_WORKSPACE = "com.samsung.e830.MEMORY_WORKSPACE"
        const val EXTRA_CARD_IMAGE = "com.samsung.e830.CARD_IMAGE"
        internal const val EXTRA_LEAVE_STARTED_AT = "com.samsung.e830.LEAVE_STARTED_AT"
        internal const val SAVE_TIMING_TAG = "Ph0nixSave"
        private const val MENU_BLUETOOTH = 2
        private const val MENU_CARDS = 3
        private const val MENU_DEVELOPER = 4
    }
}
