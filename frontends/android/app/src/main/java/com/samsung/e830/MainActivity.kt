package com.samsung.e830

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.text.InputFilter
import android.view.LayoutInflater
import android.view.Menu
import android.view.MenuItem
import android.view.View
import android.view.WindowManager
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.appcompat.widget.Toolbar
import androidx.appcompat.widget.SearchView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.lifecycleScope
import androidx.recyclerview.widget.GridLayoutManager
import androidx.recyclerview.widget.LinearLayoutManager
import androidx.recyclerview.widget.RecyclerView
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import com.google.android.material.floatingactionbutton.FloatingActionButton
import com.google.android.material.snackbar.Snackbar
import com.google.android.material.tabs.TabLayout
import com.google.android.material.button.MaterialButton
import com.google.android.material.switchmaterial.SwitchMaterial
import java.io.File
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : AppCompatActivity() {
    private lateinit var library: FirmwareLibrary
    private lateinit var adapter: FirmwareAdapter
    private lateinit var recycler: RecyclerView
    private lateinit var emptyState: View
    private lateinit var status: TextView
    private lateinit var importButton: FloatingActionButton
    private var pendingNandEntryId: String? = null
    private var pendingMemoryEntryId: String? = null
    private var pendingCardEntryId: String? = null
    private var importDevice = "k750i"
    private var busy = false
    private lateinit var preferences: AppPreferences
    private lateinit var tabs: TabLayout
    private var entries = emptyList<FirmwareEntry>()
    private var query = ""

    private val importFirmware = registerForActivityResult(
        ActivityResultContracts.OpenMultipleDocuments(),
    ) { uris ->
        if (!uris.isNullOrEmpty()) importFirmwareFiles(uris)
    }

    private val selectOneNand = registerForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri ->
        val entryId = pendingNandEntryId
        pendingNandEntryId = null
        if (uri != null && entryId != null) attachOneNand(entryId, uri)
    }

    private val exportMemoryDocument = registerForActivityResult(
        ActivityResultContracts.CreateDocument("application/octet-stream"),
    ) { uri -> finishMemorySelection(uri, importing = false) }

    private val importMemoryDocument = registerForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri -> finishMemorySelection(uri, importing = true) }

    private val importBootCard = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        val id = pendingCardEntryId
        pendingCardEntryId = null
        if (uri != null && id != null && !busy) prepareBootCard(id, uri)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        preferences = AppPreferences(this)
        pendingNandEntryId = savedInstanceState?.getString(STATE_NAND_ENTRY)
        pendingMemoryEntryId = savedInstanceState?.getString(STATE_MEMORY_ENTRY)
        pendingCardEntryId = savedInstanceState?.getString(STATE_CARD_ENTRY)
        importDevice = savedInstanceState?.getString(STATE_IMPORT_DEVICE)
            ?.takeIf { DeviceProfiles.find(it) != null } ?: importDevice
        WindowCompat.setDecorFitsSystemWindows(window, false)
        setContentView(R.layout.activity_main)
        setSupportActionBar(findViewById<Toolbar>(R.id.toolbar))
        val root = requireNotNull(findViewById<View>(R.id.root))
        applySystemInsets(root)
        WindowInsetsControllerCompat(window, root).isAppearanceLightStatusBars = false
        WindowInsetsControllerCompat(window, root).isAppearanceLightNavigationBars = false

        library = FirmwareLibrary.get(this)
        recycler = requireNotNull(findViewById<RecyclerView>(R.id.recycler))
        emptyState = requireNotNull(findViewById<View>(R.id.empty_state))
        status = requireNotNull(findViewById<TextView>(R.id.status))
        importButton = requireNotNull(findViewById<FloatingActionButton>(R.id.fab_import))
        adapter = FirmwareAdapter(
            onRun = ::runFirmware,
            onRename = ::showRenameDialog,
            onSelectOneNand = ::chooseOneNand,
            onUseErasedFlash = ::useAutomaticNand,
            onExportMemory = ::chooseMemoryExport,
            onImportMemory = ::confirmMemoryImport,
            onRemove = ::confirmRemove,
        )
        recycler.adapter = adapter
        applyLibraryView()
        tabs = findViewById(R.id.tabs)
        listOf("Library", "Devices", "Settings").forEach { tabs.addTab(tabs.newTab().setText(it)) }
        tabs.addOnTabSelectedListener(object : TabLayout.OnTabSelectedListener {
            override fun onTabSelected(tab: TabLayout.Tab) { showTab(tab.position) }
            override fun onTabUnselected(tab: TabLayout.Tab) {}
            override fun onTabReselected(tab: TabLayout.Tab) {}
        })
        buildDevices()
        buildSettings()
        tabs.getTabAt(savedInstanceState?.getInt("tab", 0) ?: 0)?.select()
        importButton.setOnClickListener {
            val profiles = DeviceProfiles.all.sortedBy { if (it.id == "k750i") 0 else 1 }
            com.google.android.material.dialog.MaterialAlertDialogBuilder(this)
                .setTitle(R.string.choose_phone)
                .setItems(profiles.map { it.name }.toTypedArray()) { _, index ->
                    importDevice = profiles[index].id
                    status.text = profiles[index].selectionHint
                    importFirmware.launch(arrayOf("*/*"))
                }.show()
        }
    }

    override fun onResume() {
        super.onResume()
        refreshLibrary()
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putString(STATE_NAND_ENTRY, pendingNandEntryId)
        outState.putString(STATE_MEMORY_ENTRY, pendingMemoryEntryId)
        outState.putString(STATE_CARD_ENTRY, pendingCardEntryId)
        outState.putString(STATE_IMPORT_DEVICE, importDevice)
        outState.putInt("tab", tabs.selectedTabPosition)
        super.onSaveInstanceState(outState)
    }

    private fun applySystemInsets(view: View) {
        val initialLeft = view.paddingLeft
        val initialTop = view.paddingTop
        val initialRight = view.paddingRight
        val initialBottom = view.paddingBottom
        ViewCompat.setOnApplyWindowInsetsListener(view) { target, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
            target.setPadding(
                initialLeft + bars.left,
                initialTop + bars.top,
                initialRight + bars.right,
                initialBottom + bars.bottom,
            )
            insets
        }
        ViewCompat.requestApplyInsets(view)
    }

    private fun gridSpanCount(): Int = (resources.configuration.screenWidthDp / 150).coerceAtLeast(2)

    override fun onCreateOptionsMenu(menu: Menu): Boolean {
        menuInflater.inflate(R.menu.main, menu)
        (menu.findItem(R.id.action_search).actionView as SearchView).apply {
            queryHint = "Search firmware"
            setOnQueryTextListener(object : SearchView.OnQueryTextListener {
                override fun onQueryTextSubmit(text: String?): Boolean = true
                override fun onQueryTextChange(text: String?): Boolean {
                    this@MainActivity.query = text.orEmpty(); showEntries(); return true
                }
            })
        }
        return true
    }

    override fun onPrepareOptionsMenu(menu: Menu): Boolean {
        val libraryVisible = tabs.selectedTabPosition == 0
        listOf(R.id.action_search, R.id.action_view, R.id.action_sort).forEach { menu.findItem(it).isVisible = libraryVisible }
        menu.findItem(R.id.action_view).title = if (preferences.grid) "List view" else "Grid view"
        return super.onPrepareOptionsMenu(menu)
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean {
        when (item.itemId) {
            R.id.action_view -> { preferences.grid = !preferences.grid; applyLibraryView(); invalidateOptionsMenu() }
            R.id.action_sort -> com.google.android.material.dialog.MaterialAlertDialogBuilder(this)
                .setTitle("Sort firmware").setSingleChoiceItems(arrayOf("Name", "Phone"), if (preferences.sortByDevice) 1 else 0) { dialog, index ->
                    preferences.sortByDevice = index == 1; showEntries(); dialog.dismiss()
                }.show()
            R.id.action_settings -> tabs.getTabAt(2)?.select()
            R.id.action_profiles -> tabs.getTabAt(1)?.select()
            R.id.action_about -> showAbout()
            else -> return super.onOptionsItemSelected(item)
        }
        return true
    }

    private fun applyLibraryView() {
        adapter.grid = preferences.grid
        recycler.layoutManager = if (preferences.grid) GridLayoutManager(this, gridSpanCount()) else LinearLayoutManager(this)
    }

    private fun showTab(index: Int) {
        findViewById<View>(R.id.library_container).visibility = if (index == 0) View.VISIBLE else View.GONE
        findViewById<View>(R.id.devices_container).visibility = if (index == 1) View.VISIBLE else View.GONE
        findViewById<View>(R.id.settings_container).visibility = if (index == 2) View.VISIBLE else View.GONE
        invalidateOptionsMenu()
    }

    private fun showEntries() {
        val visible = entries.filter { it.name.contains(query, true) || it.profile.name.contains(query, true) }
            .sortedBy { (if (preferences.sortByDevice) it.profile.name + " " else "") + it.name.lowercase(java.util.Locale.ROOT) }
        adapter.submit(visible)
        recycler.visibility = if (visible.isEmpty()) View.GONE else View.VISIBLE
        emptyState.visibility = if (visible.isEmpty()) View.VISIBLE else View.GONE
        findViewById<TextView>(R.id.empty_title).text = if (entries.isNotEmpty() && visible.isEmpty()) "No matching firmware" else getString(R.string.empty_title)
    }

    private fun buildDevices() {
        val content = findViewById<LinearLayout>(R.id.devices_content)
        content.addView(infoText("Available phone profiles", true))
        DeviceProfiles.all.sortedBy { if (it.id == "k750i") 0 else 1 }.forEach { profile ->
            content.addView(infoText(profile.name, true))
            content.addView(infoText(profile.support))
            content.addView(MaterialButton(this).apply {
                text = "Import for ${profile.name}"; isAllCaps = false
                setOnClickListener {
                    if (!busy) {
                        importDevice = profile.id
                        tabs.getTabAt(0)?.select()
                        status.text = profile.selectionHint
                        importFirmware.launch(arrayOf("*/*"))
                    }
                }
            })
        }
        content.addView(infoText("Manufacturer roadmap", true))
        val catalog = JSONArray(NativeBridge.nativeCatalog())
        for (index in 0 until catalog.length()) {
            val item = catalog.getJSONObject(index)
            content.addView(infoText("${item.getString("name")} · ${item.getString("status")}", true))
            content.addView(infoText(item.getString("detail")))
        }
    }

    private fun buildSettings() {
        val content = findViewById<LinearLayout>(R.id.settings_content)
        content.addView(infoText("Display", true))
        content.addView(settingSwitch("Show FPS", preferences.showFps) { preferences.showFps = it })
        content.addView(settingSwitch("Sound", preferences.sound) { preferences.sound = it })
        content.addView(settingSwitch("Open phone in fullscreen", preferences.fullscreen) { preferences.fullscreen = it })
        content.addView(settingSwitch("Keep screen awake while running", preferences.keepAwake) { preferences.keepAwake = it })
        content.addView(infoText("Phone state", true))
        content.addView(settingSwitch("Resume saved K750i session", preferences.resume) { preferences.resume = it })
        content.addView(infoText("When enabled, K750i continues from its saved session. Disabling it cold boots with saved internal storage and settings. Internal storage is saved when the phone stops. First boot can take several minutes."))
        content.addView(infoText("Presentation targets 60 FPS. Firmware LCD updates and emulation speed depend on the phone and your device."))
    }

    private fun settingSwitch(label: String, checked: Boolean, change: (Boolean) -> Unit) = SwitchMaterial(this).apply {
        showText = false
        text = label; isChecked = checked
        setPadding(0, 12, 0, 12)
        setOnCheckedChangeListener { _, enabled -> change(enabled) }
    }

    private fun infoText(value: String, heading: Boolean = false) = TextView(this).apply {
        text = value; textSize = if (heading) 18f else 14f
        setPadding(0, if (heading) 24 else 8, 0, 8)
        if (heading) setTypeface(typeface, android.graphics.Typeface.BOLD)
    }

    private fun showAbout() {
        val notices = assets.open("licenses/NOTICE.txt").bufferedReader().use { it.readText() }
        com.google.android.material.dialog.MaterialAlertDialogBuilder(this)
            .setTitle("Ph0nix ${packageManager.getPackageInfo(packageName, 0).versionName}")
            .setMessage("Native firmware emulator for Android, Windows and Linux.\n\n$notices")
            .setPositiveButton("Close", null).show()
    }

    private fun refreshLibrary() {
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) { runCatching { library.entries() } }
            result.fold(
                onSuccess = { loaded ->
                    entries = loaded
                    showEntries()
                },
                onFailure = { error ->
                    recycler.visibility = View.GONE
                    emptyState.visibility = View.VISIBLE
                    status.text = getString(R.string.library_error, error.message ?: "unknown error")
                },
            )
        }
    }

    private fun importFirmwareFiles(uris: List<Uri>) {
        if (busy) return
        setBusy(true)
        status.setText(R.string.import_in_progress)
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching { library.importFirmware(uris, importDevice) }
            }
            setBusy(false)
            result.fold(
                onSuccess = { entry ->
                    if (entry.device == "k750i") notifyPhoneStorageChanged(this@MainActivity)
                    status.text = getString(R.string.import_success, entry.name)
                    refreshLibrary()
                },
                onFailure = { error ->
                    status.text = getString(R.string.source_files_untouched)
                    showMessage(getString(R.string.import_failed, error.message ?: "unknown error"))
                },
            )
        }
    }

    private fun chooseOneNand(entry: FirmwareEntry) {
        if (busy || !entry.profile.hasNand) return
        pendingNandEntryId = entry.id
        selectOneNand.launch(arrayOf("*/*"))
    }

    private fun attachOneNand(entryId: String, uri: Uri) {
        if (busy) return
        setBusy(true)
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    val entry = library.entries().firstOrNull { it.id == entryId }
                        ?: throw IllegalStateException("This firmware entry no longer exists.")
                    library.attachOneNand(entry, uri)
                }
            }
            setBusy(false)
            result.fold(
                onSuccess = {
                    status.setText(R.string.nand_present)
                    refreshLibrary()
                },
                onFailure = { error -> showMessage(error.message ?: "Could not use this NAND image.") },
            )
        }
    }

    private fun useAutomaticNand(entry: FirmwareEntry) {
        if (busy) return
        setBusy(true)
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching { library.useAutomaticNand(entry) }
            }
            setBusy(false)
            result.fold(
                onSuccess = {
                    status.setText(R.string.nand_missing)
                    refreshLibrary()
                },
                onFailure = { error -> showMessage(error.message ?: "Could not remove the imported NAND copy.") },
            )
        }
    }

    private fun chooseMemoryExport(entry: FirmwareEntry) {
        if (busy) return
        pendingMemoryEntryId = entry.id
        exportMemoryDocument.launch("phone-memory.k750mem")
    }

    private fun confirmMemoryImport(entry: FirmwareEntry) {
        if (busy) return
        com.google.android.material.dialog.MaterialAlertDialogBuilder(this)
            .setTitle(R.string.k750_memory_import_title)
            .setMessage(R.string.k750_memory_import_confirm)
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.k750_memory_choose) { _, _ ->
                pendingMemoryEntryId = entry.id
                importMemoryDocument.launch(arrayOf("*/*"))
            }
            .show()
    }

    private fun finishMemorySelection(uri: Uri?, importing: Boolean) {
        val entryId = pendingMemoryEntryId
        pendingMemoryEntryId = null
        if (uri == null || entryId == null || busy) return
        setBusy(true)
        status.setText(R.string.k750_memory_working)
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    val entry = library.entries().firstOrNull { it.id == entryId && it.device == "k750i" }
                        ?: error(getString(R.string.k750_memory_missing_entry))
                    val staged = File.createTempFile("k750-memory-", ".k750mem", cacheDir)
                    try {
                        if (importing) {
                            val source = contentResolver.openInputStream(uri)
                                ?: error(getString(R.string.k750_memory_open_failed))
                            source.use { input ->
                                staged.outputStream().use { output ->
                                    val buffer = ByteArray(64 * 1024)
                                    var total = 0L
                                    while (true) {
                                        val count = input.read(buffer)
                                        if (count == -1) break
                                        total += count
                                        check(total <= MAX_MEMORY_FILE_BYTES) { getString(R.string.k750_memory_too_large) }
                                        output.write(buffer, 0, count)
                                    }
                                }
                            }
                        }
                        val failure = NativeBridge.nativeTransferInternalMemory(
                            entry.selectionJson(), File(filesDir, "sessions").absolutePath,
                            entry.memoryWorkspace?.absolutePath.orEmpty(), staged.absolutePath, importing,
                        )
                        check(failure.isEmpty()) { failure }
                        if (!importing) {
                            val destination = contentResolver.openOutputStream(uri, "wt")
                                ?: error(getString(R.string.k750_memory_open_failed))
                            destination.use { output -> staged.inputStream().use { it.copyTo(output) } }
                        }
                    } finally {
                        staged.delete()
                    }
                }
            }
            setBusy(false)
            result.fold(
                onSuccess = {
                    status.setText(if (importing) R.string.k750_memory_imported else R.string.k750_memory_exported)
                },
                onFailure = { error ->
                    val message = getString(R.string.k750_memory_failed, error.localizedMessage ?: "")
                    status.text = message
                    showMessage(message)
                },
            )
        }
    }

    private fun runFirmware(entry: FirmwareEntry) {
        if (busy) return
        if (entry.device != "k750i") {
            launchFirmware(entry, null)
            return
        }
        val cards = File(filesDir, "cards").listFiles()
            ?.filter { it.isFile && it.extension.equals("img", ignoreCase = true) }
            ?.sortedWith(compareBy<File> { it.name != preferences.phoneCard(entry.id) }.thenBy { it.name }).orEmpty()
        val choices = listOf("Start without a card", "Create a card and start…", "Import an image and start…") +
            cards.map { "${it.name} (${it.length() / 1048576} MiB)" }
        MaterialAlertDialogBuilder(this).setTitle("K750i Memory Stick")
            .setItems(choices.toTypedArray()) { _, index ->
                when (index) {
                    0 -> launchFirmware(entry, null)
                    1 -> chooseBootCardSize(entry.id)
                    2 -> {
                        pendingCardEntryId = entry.id
                        importBootCard.launch(arrayOf("*/*"))
                    }
                    else -> launchFirmware(entry, cards[index - 3])
                }
            }.setNegativeButton(R.string.cancel, null).show()
    }

    private fun chooseBootCardSize(entryId: String) {
        val sizes = listOf(16, 32, 64, 128, 256, 512, 1024)
        val labels = sizes.map { if (it == 1024) "1 GiB" else "$it MiB" }.toTypedArray()
        MaterialAlertDialogBuilder(this).setTitle("Choose Memory Stick size")
            .setItems(labels) { _, index -> prepareBootCard(entryId, source = null, sizeMib = sizes[index]) }
            .setNegativeButton(R.string.cancel, null).show()
    }

    private fun prepareBootCard(entryId: String, source: Uri?, sizeMib: Int? = null) {
        if (busy) return
        setBusy(true)
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    val entry = library.entries().firstOrNull { it.id == entryId && it.device == "k750i" }
                        ?: error(getString(R.string.k750_memory_missing_entry))
                    val directory = File(filesDir, "cards")
                    check(directory.isDirectory || directory.mkdirs()) { "Could not create the card images folder." }
                    val image = File(directory, "card-${UUID.randomUUID()}.img")
                    try {
                        if (source != null) {
                            val input = contentResolver.openInputStream(source) ?: error("Could not open the card image.")
                            input.use {
                                image.outputStream().use { output ->
                                    val buffer = ByteArray(64 * 1024)
                                    var total = 0L
                                    while (true) {
                                        val count = input.read(buffer)
                                        if (count == -1) break
                                        total += count
                                        check(total <= 1024L * 1024 * 1024) { "The card image exceeds 1 GiB." }
                                        output.write(buffer, 0, count)
                                    }
                                }
                            }
                        }
                        val request = JSONObject().put("image", image.absolutePath)
                            .put("operation", if (source == null) "create" else "info")
                        if (source == null) request.put("size_mib", requireNotNull(sizeMib))
                        val response = JSONObject(NativeBridge.nativeCardRequest(request.toString()))
                        check(!response.has("error")) { response.optString("error") }
                    } catch (error: Exception) {
                        image.delete()
                        throw error
                    }
                    entry to image
                }
            }
            setBusy(false)
            result.onSuccess { (entry, image) -> launchFirmware(entry, image) }
                .onFailure { showMessage(it.message ?: "Could not prepare the Memory Stick.") }
        }
    }

    private fun launchFirmware(entry: FirmwareEntry, card: File?) {
        if (entry.device == "k750i" && card != null) {
            val previous = preferences.phoneCard(entry.id)
            if (previous != card.name) {
                preferences.rememberPhoneCard(entry.id, card.name)
                notifyMemoryStickSelectionChanged(this, entry.id, previous)
            }
        }
        startActivity(
            Intent(this, EmulatorActivity::class.java)
                .putExtra(EmulatorActivity.EXTRA_SELECTION, entry.selectionJson())
                .putExtra(EmulatorActivity.EXTRA_PHONE_ID, entry.id)
                .putExtra(EmulatorActivity.EXTRA_PHONE_DIRECTORY, entry.directory.absolutePath)
                .putExtra(EmulatorActivity.EXTRA_MEMORY_WORKSPACE, entry.memoryWorkspace?.absolutePath)
                .putExtra(EmulatorActivity.EXTRA_CARD_IMAGE, card?.absolutePath),
        )
    }

    private fun showRenameDialog(entry: FirmwareEntry) {
        if (busy) return
        val builder = MaterialAlertDialogBuilder(this)
        val view = LayoutInflater.from(builder.context).inflate(R.layout.dialog_rename_phone, null)
        val input = view.findViewById<EditText>(R.id.phone_name)
        input.filters = arrayOf(InputFilter.LengthFilter(FirmwareLibrary.MAX_NAME_LENGTH))
        input.setText(entry.name)
        val dialog = builder.setTitle(R.string.rename_phone_title)
            .setView(view)
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.rename, null)
            .create()
        input.requestFocus()
        input.selectAll()
        dialog.window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_VISIBLE)
        dialog.show()
        dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener {
            val name = input.text.toString().trim()
            if (name.isEmpty()) {
                input.error = getString(R.string.phone_name_required)
            } else {
                dialog.dismiss()
                if (name != entry.name) renamePhone(entry.id, name)
            }
        }
    }

    private fun renamePhone(id: String, name: String) {
        if (busy) return
        setBusy(true)
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) { runCatching { library.rename(id, name) } }
            setBusy(false)
            result.onSuccess {
                refreshLibrary()
                notifyPhoneStorageChanged(this@MainActivity)
                showMessage(getString(R.string.phone_renamed, name))
            }.onFailure { error ->
                showMessage(error.message ?: getString(R.string.rename_phone_failed))
            }
        }
    }

    private fun confirmRemove(entry: FirmwareEntry) {
        if (busy) return
        com.google.android.material.dialog.MaterialAlertDialogBuilder(this)
            .setTitle(R.string.remove_title)
            .setMessage(getString(R.string.remove_message, entry.name))
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.remove) { _, _ -> removeFirmware(entry) }
            .show()
    }

    private fun removeFirmware(entry: FirmwareEntry) {
        if (busy) return
        setBusy(true)
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) { runCatching { library.remove(entry) } }
            setBusy(false)
            result.fold(
                onSuccess = {
                    if (entry.device == "k750i") {
                        preferences.rememberPhoneCard(entry.id, null)
                        notifyPhoneStorageChanged(this@MainActivity)
                    }
                    status.setText(R.string.source_files_untouched)
                    refreshLibrary()
                },
                onFailure = { error -> showMessage(error.message ?: "Could not remove this firmware set.") },
            )
        }
    }

    private fun setBusy(busy: Boolean) {
        this.busy = busy
        importButton.isEnabled = !busy
        recycler.isEnabled = !busy
        recycler.alpha = if (busy) 0.55f else 1f
    }

    private fun showMessage(message: String) {
        Snackbar.make(requireNotNull(findViewById<View>(R.id.root)), message, Snackbar.LENGTH_LONG).show()
    }

    private companion object {
        const val STATE_NAND_ENTRY = "pendingNandEntryId"
        const val STATE_MEMORY_ENTRY = "pendingMemoryEntryId"
        const val STATE_CARD_ENTRY = "pendingCardEntryId"
        const val MAX_MEMORY_FILE_BYTES = 65L * 1024 * 1024
        const val STATE_IMPORT_DEVICE = "importDevice"
    }
}
