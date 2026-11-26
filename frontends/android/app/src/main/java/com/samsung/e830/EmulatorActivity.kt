package com.samsung.e830

import android.net.Uri
import android.Manifest
import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.widget.GridLayout
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
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
import java.security.MessageDigest
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
    private var fullscreen = false
    private var bindings = emptyList<Binding>()
    private var developerPanel: DeveloperPanel? = null
    private var diagnosticsDueNs = 0L
    private var recordedPresentations = 0L
    @Volatile private var audioProblem: String? = null
    private var filePanel: PhoneFilePanel? = null
    private var latestFiles = JSONObject()
    private val pendingFiles = ArrayDeque<PendingFile>()
    private var activeFile: PendingFile? = null
    private var activeFileId = 0L
    private var lastFileErrorId = 0L
    private var importDestination = emptyList<String>()
    private var moveImportedDocument = false
    private var importCardImage: String? = null
    private var documentExport: File? = null
    private var documentExportTemporary = true
    private var exportPhoneSource = emptyList<String>()
    private var exportCardImage: String? = null
    private var deleteExportedPhoneSource = false
    private var cardPanel: PhoneFilePanel? = null
    private var selectedCardImage: String? = null
    private var cardBusy = false
    private lateinit var bluetooth: BluetoothBridge
    private var bluetoothStatus: TextView? = null
    private var bluetoothMessage = "Uses this device's Bluetooth adapter to transfer files to and from the phone. Pair devices in Android settings first."
    private val receivedBluetooth = mutableMapOf<String, JSONObject>()
    private var afterBluetoothPermission: (() -> Unit)? = null
    private val bluetoothPermission = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { result ->
        val action = afterBluetoothPermission
        afterBluetoothPermission = null
        if (result.values.all { it }) action?.invoke()
        else Toast.makeText(this, "Bluetooth permission is needed to transfer files.", Toast.LENGTH_LONG).show()
    }
    private val discoverable = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        if (it.resultCode > 0) runCatching { bluetooth.receive() }.onFailure { showBluetoothMessage(it.message.orEmpty()) }
    }
    private val importDocument = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) importPhoneDocument(uri, importDestination, moveImportedDocument, importCardImage)
    }
    private val importCardDocument = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) fileExecutor.execute {
            val image = File(filesDir, "cards/imported-${UUID.randomUUID()}.img")
            val result = runCatching {
                image.parentFile!!.mkdirs()
                contentResolver.openInputStream(uri)!!.use { input -> image.outputStream().use { copyLimited(input, it, 1024L * 1024L * 1024L) } }
                val response = JSONObject(NativeBridge.nativeCardRequest(JSONObject().put("image", image.absolutePath).put("operation", "info").toString()))
                check(!response.has("error")) { response.optString("error") }
            }
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                result.onSuccess { showCardFiles(image.absolutePath) }
                    .onFailure { image.delete(); Toast.makeText(this, "Card import failed: ${it.message}", Toast.LENGTH_LONG).show() }
            }
        }
    }
    private val exportDocument = registerForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        val staged = documentExport
        val temporary = documentExportTemporary
        documentExport = null
        if (uri != null && staged != null) {
            val source = exportPhoneSource.toList()
            val move = deleteExportedPhoneSource
            val card = exportCardImage
            fileExecutor.execute {
                var expectedBytes = 0L
                var expectedSha = ""
                val result = runCatching {
                    contentResolver.openOutputStream(uri, "wt")!!.use { target -> staged.inputStream().use { it.copyTo(target, 32768) } }
                    if (move) {
                        val expected = staged.inputStream().use(::streamDigest)
                        val actual = contentResolver.openInputStream(uri)!!.use(::streamDigest)
                        check(expected.contentEquals(actual)) { "Export verification failed. The phone's source was kept." }
                        expectedBytes = staged.length(); expectedSha = hexDigest(expected)
                    }
                }
                if (temporary) staged.delete()
                runOnUiThread {
                    if (isFinishing || isDestroyed) return@runOnUiThread
                    result.onSuccess {
                        filePanel?.message("File exported.")
                        if (move) dispatchFile(card, JSONObject().put("operation", "delete_verified").put("path", JSONArray(source))
                            .put("bytes", expectedBytes).put("sha256", expectedSha))
                    }.onFailure { filePanel?.message("Export failed: ${it.message}") }
                }
            }
        } else if (temporary) staged?.delete()
    }
    private val selection by lazy { intent.getStringExtra(EXTRA_SELECTION).orEmpty() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        preferences = AppPreferences(this)
        savedInstanceState?.let { saved ->
            importDestination = saved.getStringArrayList("import_destination").orEmpty()
            moveImportedDocument = saved.getBoolean("move_import_document")
            importCardImage = saved.getString("import_card_image")
            documentExport = saved.getString("document_export")?.let(::File)
            documentExportTemporary = saved.getBoolean("document_export_temporary", true)
            exportPhoneSource = saved.getStringArrayList("export_phone_source").orEmpty()
            exportCardImage = saved.getString("export_card_image")
            deleteExportedPhoneSource = saved.getBoolean("delete_export_phone_source")
        }
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
                    val destination = filePanel?.currentPath()?.takeIf { it.isNotEmpty() } ?: listOf("Phone memory", "Other")
                    MaterialAlertDialogBuilder(this).setTitle("Import ${file.optString("name")}?")
                        .setMessage("Received ${file.optLong("bytes")} bytes via Bluetooth. Import into /${destination.joinToString("/")}/.")
                        .setNegativeButton("Keep in inbox", null).setPositiveButton("Import") { _, _ ->
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
        toolbar.menu.add(0, MENU_FILES, 0, "Phone files")
        toolbar.menu.add(0, MENU_BLUETOOTH, 1, "Bluetooth files")
        toolbar.menu.add(0, MENU_CARDS, 2, "Card images")
        toolbar.menu.add(0, MENU_DEVELOPER, 3, "Developer")
        toolbar.setOnMenuItemClickListener { item ->
            when (item.itemId) {
                MENU_FILES -> showFileBrowser()
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
        findViewById<View>(R.id.btn_back).setOnClickListener { finish() }
        lcd.setOnClickListener { if (fullscreen) setFullscreen(false) }
        setFullscreen(savedInstanceState?.getBoolean("fullscreen") ?: preferences.fullscreen)
        if (selection.isBlank()) finish()
    }

    override fun onStart() {
        super.onStart()
        root.keepScreenOn = preferences.keepAwake
        if (selection.isNotBlank()) startSession()
    }

    override fun onStop() {
        root.keepScreenOn = false
        stopSession()
        super.onStop()
    }

    override fun onDestroy() {
        stopSession()
        developerPanel?.dismiss()
        filePanel?.dismiss()
        cardPanel?.dismiss()
        bluetooth.close()
        nativeExecutor.shutdown() // Let queued checkpoint saves finish.
        fileExecutor.shutdown()
        lcd.releaseFrame()
        super.onDestroy()
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean("fullscreen", fullscreen)
        outState.putStringArrayList("import_destination", ArrayList(importDestination))
        outState.putBoolean("move_import_document", moveImportedDocument)
        outState.putString("import_card_image", importCardImage)
        outState.putString("document_export", documentExport?.absolutePath)
        outState.putBoolean("document_export_temporary", documentExportTemporary)
        outState.putStringArrayList("export_phone_source", ArrayList(exportPhoneSource))
        outState.putString("export_card_image", exportCardImage)
        outState.putBoolean("delete_export_phone_source", deleteExportedPhoneSource)
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

    private fun startSession() {
        val token = generation.incrementAndGet()
        audioPlayer?.close()
        audioProblem = null
        val player = if (preferences.sound) AudioPlayer { error ->
            if (generation.get() == token) {
                audioProblem = error
                val active = sessionHandle.get()
                if (active != 0L) nativeExecutor.execute {
                    if (generation.get() == token && sessionHandle.get() == active)
                        NativeBridge.nativeLog(active, "error", "android-audio", error)
                }
            }
        } else null
        audioPlayer = player
        status.setText(R.string.emulator_starting)
        waiting.visibility = View.VISIBLE
        nativeExecutor.execute {
            val handle = try {
                NativeBridge.nativeConfigure(
                    File(applicationInfo.nativeLibraryDir, "libdb2010_engine.so").absolutePath,
                    File(filesDir, "sessions").absolutePath, preferences.resume,
                )
                NativeBridge.nativeStartSelection(selection, intent.getStringExtra(EXTRA_MEMORY_WORKSPACE).orEmpty()).also {
                    check(it != 0L) { "Could not start the phone." }
                }
            } catch (error: Throwable) {
                player?.close()
                runOnUiThread {
                    if (generation.get() == token) {
                        audioPlayer = null
                        waiting.visibility = View.GONE
                        status.text = error.message ?: "Could not start the phone."
                    }
                }
                return@execute
            }
            if (generation.get() != token) { player?.close(); stopAndDestroy(handle); return@execute }
            sessionHandle.set(handle)
            diagnosticsDueNs = 0L
            audioProblem?.let { NativeBridge.nativeLog(handle, "error", "android-audio", it) }
            runOnUiThread {
                if (generation.get() == token && sessionHandle.get() == handle) {
                    recordedPresentations = lcd.presentationCount
                    latestFiles = JSONObject()
                    lcd.onPresentationFrame = { pollNative(token, handle) }
                    lcd.setRunning(true)
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
                        if (status.text.toString() != message) status.text = message
                        frame?.let { lcd.setFrame(it); waiting.visibility = View.GONE }
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
                        status.text = error.message ?: "The phone stopped unexpectedly."
                        stopSession()
                    }
                }
            } finally { pollPending.set(false) }
        }
    }

    private fun stopSession() {
        releaseKeys()
        generation.incrementAndGet()
        lcd.setRunning(false)
        lcd.onPresentationFrame = null
        val handle = sessionHandle.getAndSet(0)
        activeFile?.let {
            filePanel?.message("Transfer interrupted while the phone was stopped. Retry after it resumes.")
            activeFile = null; activeFileId = 0L
        }
        audioPlayer?.close()
        audioPlayer = null
        if (handle != 0L) {
            val stoppedGeneration = generation.get()
            nativeExecutor.execute { stopAndDestroy(handle, stoppedGeneration) }
        }
    }

    private fun openDiagnostics() {
        releaseKeys()
        val token = generation.get()
        val handle = sessionHandle.get()
        if (handle == 0L) return
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

    private fun showDiagnostics(snapshot: JSONObject, token: Long, handle: Long) {
        if (developerPanel?.isShowing == true) {
            developerPanel?.update(snapshot)
        } else if (snapshot.optBoolean("developer_mode")) {
            releaseKeys()
            developerPanel = DeveloperPanel(this) {
                if (generation.get() == token && sessionHandle.get() == handle) nativeExecutor.execute {
                    if (generation.get() == token && sessionHandle.get() == handle)
                        NativeBridge.nativeSetDeveloperMode(handle, false)
                }
            }.also { it.show(snapshot) }
        }
    }

    private fun showFileBrowser() {
        releaseKeys()
        if (filePanel?.isShowing == true) return
        filePanel = PhoneFilePanel(this, { enqueueFile(it) }, { destination, move ->
            importDestination = destination; moveImportedDocument = move; importCardImage = null
            importDocument.launch(arrayOf("*/*"))
        }, { source, move -> stagePhoneExport(source, move, false) },
            { source -> stagePhoneExport(source, false, true) }).also { it.show() }
    }

    private fun enqueueFile(request: JSONObject, completed: ((JSONObject) -> Unit)? = null): Boolean {
        if (isFinishing || isDestroyed) return false
        if (pendingFiles.size >= 8) {
            filePanel?.message("The transfer queue is full. Wait for a transfer to finish.")
            return false
        }
        pendingFiles.addLast(PendingFile(request, completed))
        drainFiles()
        return true
    }

    private fun drainFiles() {
        if (activeFile != null || pendingFiles.isEmpty() || !latestFiles.optBoolean("attached") || latestFiles.optBoolean("busy")) return
        val handle = sessionHandle.get(); val token = generation.get()
        if (handle == 0L) return
        val action = pendingFiles.removeFirst()
        activeFile = action
        nativeExecutor.execute {
            if (sessionHandle.get() != handle || generation.get() != token) {
                runOnUiThread { if (activeFile === action) { activeFile = null; pendingFiles.addFirst(action) } }
                return@execute
            }
            val result = runCatching { JSONObject(NativeBridge.nativeFileRequest(handle, action.request.toString())) }
            runOnUiThread {
                if (activeFile !== action) return@runOnUiThread
                result.onSuccess { response ->
                    if (response.has("error")) { filePanel?.message(response.optString("error")); activeFile = null }
                    else activeFileId = response.getLong("id")
                }.onFailure { filePanel?.message(it.message.orEmpty()); activeFile = null }
            }
        }
    }

    private fun updateFiles(snapshot: JSONObject) {
        latestFiles = snapshot
        filePanel?.update(snapshot)
        val job = snapshot.optJSONObject("job")
        if (job != null && activeFileId != 0L && job.optLong("id") == activeFileId && !snapshot.optBoolean("busy")) {
            val action = activeFile
            activeFile = null; activeFileId = 0L
            if (job.optString("phase") == "complete" && (job.isNull("error") || !job.has("error"))) {
                action?.completed?.invoke(job)
                if (filePanel?.isShowing == true && action?.request?.optString("operation") in
                    setOf("import", "copy", "delete", "delete_verified", "make_directory")) {
                    enqueueFile(JSONObject().put("operation", "list").put("path", JSONArray(filePanel!!.currentPath())))
                }
            }
            else if (job.isNull("error") || !job.has("error")) filePanel?.message("Transfer interrupted. The source was kept.")
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

    private fun importPhoneDocument(uri: Uri, destination: List<String>, move: Boolean, card: String? = null) {
        fileExecutor.execute {
            var staging: File? = null
            var name = "imported-file"
            var expected = ByteArray(0)
            val result = runCatching {
                val displayName = contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
                    if (cursor.moveToFirst()) cursor.getString(0) else null
                } ?: name
                name = displayName.replace('/', '_').replace('\\', '_').replace('\u0000', '_').take(180)
                    .takeUnless { it.isBlank() || it == "." || it == ".." } ?: "imported-file"
                val target = File(filesDir, "transfer-staging/${UUID.randomUUID()}/$name")
                staging = target; target.parentFile!!.mkdirs()
                contentResolver.openInputStream(uri)!!.use { input -> target.outputStream().use { copyLimited(input, it, 32L * 1024L * 1024L) } }
                if (move) expected = target.inputStream().use(::streamDigest)
            }
            runOnUiThread {
                if (isFinishing || isDestroyed) { staging?.delete(); return@runOnUiThread }
                result.onSuccess {
                    val accepted = dispatchFile(card, JSONObject().put("operation", "import").put("path", JSONArray(destination + name))
                        .put("host", staging!!.absolutePath).put("replace", false).put("move_source", true)) {
                        if (move) fileExecutor.execute {
                            val result = runCatching {
                                val current = contentResolver.openInputStream(uri)!!.use(::streamDigest)
                                check(expected.contentEquals(current)) { "Source changed during the transfer. The original was kept." }
                                check(DocumentsContract.deleteDocument(contentResolver, uri)) { "The document provider could not remove the original." }
                            }
                            result.onFailure { error -> runOnUiThread { if (!isDestroyed) filePanel?.message("Imported. ${error.message}") } }
                        }
                    }
                    if (!accepted) staging?.delete()
                }.onFailure { staging?.delete(); filePanel?.message("Import failed: ${it.message}") }
            }
        }
    }

    private fun stagePhoneExport(source: List<String>, move: Boolean, bluetooth: Boolean, card: String? = null) {
        val staged = File(filesDir, "transfer-staging/${UUID.randomUUID()}/${source.last()}")
        dispatchFile(card, JSONObject().put("operation", "export").put("path", JSONArray(source))
            .put("host", staged.absolutePath).put("move_source", false)) {
            if (bluetooth) sendBluetooth(staged)
            else {
                documentExport = staged
                documentExportTemporary = true
                exportPhoneSource = source.toList(); exportCardImage = card; deleteExportedPhoneSource = move
                exportDocument.launch(source.last())
            }
        }
    }

    private fun dispatchFile(card: String?, request: JSONObject, completed: ((JSONObject) -> Unit)? = null): Boolean {
        return if (card == null) enqueueFile(request, completed) else requestCard(card, request, completed)
    }

    private fun showCards() {
        val cards = File(filesDir, "cards").listFiles()?.filter { it.isFile && it.extension.lowercase() == "img" }.orEmpty()
        val labels = listOf("Create image…", "Import image…") + cards.map { "${it.name} (${it.length() / 1048576} MiB)" }
        MaterialAlertDialogBuilder(this).setTitle("Removable card images")
            .setItems(labels.toTypedArray()) { _, index ->
                when (index) {
                    0 -> createCard()
                    1 -> importCardDocument.launch(arrayOf("*/*"))
                    else -> cardActions(cards[index - 2])
                }
            }.setNegativeButton("Close", null).show()
    }

    private fun createCard() {
        val layout = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(20), 0, dp(20), 0) }
        val name = android.widget.EditText(this).apply { hint = "Image name"; setText("ph0nix-card"); isSingleLine = true }
        val size = android.widget.EditText(this).apply { hint = "Size in MiB (16–1024)"; setText("64"); inputType = android.text.InputType.TYPE_CLASS_NUMBER }
        layout.addView(name); layout.addView(size)
        MaterialAlertDialogBuilder(this).setTitle("Create FAT16 card image").setView(layout)
            .setMessage("The image can store files. The K750i Memory Stick mount is still under development.")
            .setNegativeButton("Cancel", null).setPositiveButton("Create") { _, _ ->
                val label = name.text.toString().trim()
                val mib = size.text.toString().toLongOrNull()
                if (label.isEmpty() || label.contains('/') || label.contains('\\') || label == "." || label == ".." || mib == null || mib !in 16L..1024L) {
                    Toast.makeText(this, "Choose an image name and a size from 16 to 1024 MiB.", Toast.LENGTH_LONG).show()
                    return@setPositiveButton
                }
                val image = File(filesDir, "cards/$label.img")
                requestCard(image.absolutePath, JSONObject().put("operation", "create").put("size_mib", mib)) { showCardFiles(image.absolutePath) }
            }.show()
    }

    private fun cardActions(image: File) {
        MaterialAlertDialogBuilder(this).setTitle(image.name)
            .setMessage("FAT16 image · ${image.length() / 1048576} MiB. The K750i Memory Stick controller mount is under development.")
            .setPositiveButton("Browse files") { _, _ -> showCardFiles(image.absolutePath) }
            .setNeutralButton("Export image…") { _, _ ->
                documentExport = image; documentExportTemporary = false
                exportPhoneSource = emptyList(); exportCardImage = null; deleteExportedPhoneSource = false
                exportDocument.launch(image.name)
            }.setNegativeButton("Close", null).show()
    }

    private fun showCardFiles(image: String) {
        selectedCardImage = image
        cardPanel?.dismiss()
        cardPanel = PhoneFilePanel(this, { requestCard(image, it) }, { destination, move ->
            importDestination = destination; moveImportedDocument = move; importCardImage = image
            importDocument.launch(arrayOf("*/*"))
        }, { source, move -> stagePhoneExport(source, move, false, image) },
            { source -> stagePhoneExport(source, false, true, image) }, "Card image files").also { it.show() }
    }

    private fun requestCard(image: String, request: JSONObject, completed: ((JSONObject) -> Unit)? = null): Boolean {
        if (isFinishing || isDestroyed) return false
        if (cardBusy) { cardPanel?.message("Another card operation is in progress."); return false }
        cardBusy = true
        cardPanel?.message("Working…")
        fileExecutor.execute {
            val result = runCatching { JSONObject(NativeBridge.nativeCardRequest(request.put("image", image).toString())) }
            runOnUiThread {
                cardBusy = false
                if (isFinishing || isDestroyed) return@runOnUiThread
                result.onSuccess { response ->
                    if (response.has("error")) {
                        cardPanel?.message(response.optString("error"))
                        Toast.makeText(this, response.optString("error"), Toast.LENGTH_LONG).show()
                    } else {
                        if (selectedCardImage == image) response.optJSONObject("snapshot")?.let { cardPanel?.update(it) }
                        completed?.invoke(response)
                    }
                }.onFailure { cardPanel?.message(it.message.orEmpty()) }
            }
        }
        return true
    }

    private fun copyLimited(input: InputStream, output: OutputStream, maximum: Long) {
        val buffer = ByteArray(32768)
        var total = 0L
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            total += count
            check(total <= maximum) { "File exceeds ${maximum / 1048576} MiB limit." }
            output.write(buffer, 0, count)
        }
    }

    private fun streamDigest(input: InputStream): ByteArray {
        val hash = MessageDigest.getInstance("SHA-256")
        val buffer = ByteArray(32768)
        while (true) { val count = input.read(buffer); if (count < 0) break; hash.update(buffer, 0, count) }
        return hash.digest()
    }

    private fun hexDigest(bytes: ByteArray) = bytes.joinToString("") { (it.toInt() and 0xff).toString(16).padStart(2, '0') }

    private fun showBluetoothMessage(message: String) {
        bluetoothMessage = message
        bluetoothStatus?.text = message
    }

    private fun showBluetooth() {
        val text = TextView(this).apply { this.text = bluetoothMessage; setPadding(dp(20), dp(16), dp(20), dp(16)) }
        bluetoothStatus = text
        val layout = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; addView(text) }
        listOf("Send a phone file…", "Receive a file…", "Received files", "Import received file…", "Cancel transfer").forEachIndexed { choice, label ->
            layout.addView(MaterialButton(this, null, com.google.android.material.R.attr.materialButtonOutlinedStyle).apply {
                this.text = label; isAllCaps = false
                setOnClickListener { when (choice) {
                    0 -> showFileBrowser()
                    1 -> withBluetoothPermission(advertise = true) {
                        discoverable.launch(Intent(BluetoothAdapter.ACTION_REQUEST_DISCOVERABLE)
                            .putExtra(BluetoothAdapter.EXTRA_DISCOVERABLE_DURATION, 120))
                    }
                    2 -> showBluetoothInbox()
                    3 -> {
                        importDestination = listOf("Phone memory", "Other"); importCardImage = null; moveImportedDocument = false
                        importDocument.launch(arrayOf("*/*"))
                    }
                    4 -> bluetooth.cancel()
                } }
            })
        }
        MaterialAlertDialogBuilder(this).setTitle("Bluetooth files").setView(layout)
            .setPositiveButton("Close", null).show()
    }

    private fun showBluetoothInbox() {
        val savedRecords = File(filesDir, "bluetooth-inbox").listFiles()?.filter { it.isFile && it.name.endsWith(".json") }
            ?.mapNotNull { file -> runCatching { JSONObject(file.readText()) }.getOrNull() }
            .orEmpty()
        val records = (savedRecords + receivedBluetooth.values).distinctBy { it.optString("path") }
            .filter { File(it.optString("path")).isFile }
        if (records.isEmpty()) { Toast.makeText(this, "No received files in the inbox.", Toast.LENGTH_SHORT).show(); return }
        MaterialAlertDialogBuilder(this).setTitle("Received files").setItems(records.map { it.optString("name") }.toTypedArray()) { _, index ->
            val record = records[index]
            val destination = filePanel?.currentPath()?.takeIf { it.isNotEmpty() } ?: listOf("Phone memory", "Other")
            enqueueFile(JSONObject().put("operation", "import").put("path", JSONArray(destination + record.getString("name")))
                .put("host", record.getString("path")).put("replace", false).put("move_source", true)) {
                File(record.getString("path") + ".json").delete()
            }
        }.setNegativeButton("Close", null).show()
    }

    @SuppressLint("MissingPermission")
    private fun sendBluetooth(file: File) = withBluetoothPermission {
        val peers = runCatching { bluetooth.peers() }.getOrElse { showBluetoothMessage(it.message.orEmpty()); return@withBluetoothPermission }
        if (peers.isEmpty()) { Toast.makeText(this, "Pair the receiving device in Android settings first.", Toast.LENGTH_LONG).show(); return@withBluetoothPermission }
        MaterialAlertDialogBuilder(this).setTitle("Send ${file.name}")
            .setItems(peers.map { it.name ?: it.address }.toTypedArray()) { _, index ->
                runCatching { bluetooth.send(peers[index], file); showBluetooth() }.onFailure { showBluetoothMessage(it.message.orEmpty()) }
            }.setNegativeButton("Cancel", null).show()
    }

    private fun withBluetoothPermission(advertise: Boolean = false, action: () -> Unit) {
        val permissions = if (Build.VERSION.SDK_INT >= 31) buildList {
            add(Manifest.permission.BLUETOOTH_CONNECT)
            if (advertise) add(Manifest.permission.BLUETOOTH_ADVERTISE)
        } else emptyList()
        if (permissions.all { checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }) action()
        else { afterBluetoothPermission = action; bluetoothPermission.launch(permissions.toTypedArray()) }
    }

    private data class PendingFile(val request: JSONObject, val completed: ((JSONObject) -> Unit)?)

    private fun stopAndDestroy(handle: Long, reportGeneration: Long? = null) {
        try {
            NativeBridge.nativeSetKeys(handle, 0)
            NativeBridge.nativeStop(handle)
        } catch (error: Throwable) {
            android.util.Log.e("ph0nix", "Stopping the phone failed", error)
            runCatching { NativeBridge.nativeLog(handle, "error", "android", error.message ?: error.toString()) }
        } finally {
            // Stop joins the worker, including any pending checkpoint save.
            // Capture late errors before releasing its diagnostic handle.
            val diagnostic = if (reportGeneration != null)
                runCatching { JSONObject(NativeBridge.nativeDiagnostics(handle)) }.getOrNull() else null
            NativeBridge.nativeDestroy(handle)
            diagnostic?.let { report ->
                runOnUiThread {
                    if (generation.get() == reportGeneration && !isFinishing && !isDestroyed)
                        developerPanel?.update(report)
                }
            }
        }
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
        contentDescription = "Phone ${binding.label}"
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
        val mask = (heldTouch + heldPhysical).fold(0) { a, b -> a or b }
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
        if (binding == null || sessionHandle.get() == 0L) return super.dispatchKeyEvent(event)
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
        const val EXTRA_SELECTION = "com.samsung.e830.SELECTION"
        const val EXTRA_MEMORY_WORKSPACE = "com.samsung.e830.MEMORY_WORKSPACE"
        private const val MENU_FILES = 1
        private const val MENU_BLUETOOTH = 2
        private const val MENU_CARDS = 3
        private const val MENU_DEVELOPER = 4
    }
}
