package com.samsung.e830

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.os.SystemClock
import android.text.InputFilter
import android.util.Log
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
import androidx.core.view.doOnPreDraw
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
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
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
    private var pendingMemoryExport: File? = null
    private var pendingCardEntryId: String? = null
    private var importDevice = "k750i"
    private var busy = false
    private lateinit var preferences: AppPreferences
    private lateinit var tabs: TabLayout
    private lateinit var languageButton: MaterialButton
    private lateinit var midiSoundFontState: TextView
    private lateinit var selectMidiSoundFontButton: MaterialButton
    private lateinit var resetMidiSoundFontButton: MaterialButton
    private var entries = emptyList<FirmwareEntry>()
    private var query = ""

    private val runEmulator = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode != RESULT_OK) return@registerForActivityResult
        val start = result.data?.getLongExtra(EmulatorActivity.EXTRA_LEAVE_STARTED_AT, -1) ?: -1
        if (start < 0) return@registerForActivityResult
        window.decorView.doOnPreDraw {
            Log.i(EmulatorActivity.SAVE_TIMING_TAG, "library_pre_draw elapsed_ms=${SystemClock.elapsedRealtime() - start}")
        }
    }

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
    ) { uri -> finishMemoryExport(uri) }

    private val importMemoryDocument = registerForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri -> finishMemoryImport(uri) }

    private val selectMidiSoundFont = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) updateMidiSoundFont(uri)
    }

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
        pendingMemoryExport = savedInstanceState?.getString(STATE_MEMORY_EXPORT)?.let { File(cacheDir, it) }
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
        listOf(R.string.library, R.string.devices, R.string.settings).forEach { tabs.addTab(tabs.newTab().setText(it)) }
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
                    status.text = profiles[index].selectionHint(this)
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
        outState.putString(STATE_MEMORY_EXPORT, pendingMemoryExport?.name)
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
            queryHint = getString(R.string.search_firmware)
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
        menu.findItem(R.id.action_view).title = getString(if (preferences.grid) R.string.list_view else R.string.grid_view)
        return super.onPrepareOptionsMenu(menu)
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean {
        when (item.itemId) {
            R.id.action_view -> { preferences.grid = !preferences.grid; applyLibraryView(); invalidateOptionsMenu() }
            R.id.action_sort -> com.google.android.material.dialog.MaterialAlertDialogBuilder(this)
                .setTitle(R.string.sort_firmware).setSingleChoiceItems(
                    arrayOf(getString(R.string.sort_name), getString(R.string.sort_phone)), if (preferences.sortByDevice) 1 else 0,
                ) { dialog, index ->
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
        findViewById<TextView>(R.id.empty_title).setText(
            if (entries.isNotEmpty() && visible.isEmpty()) R.string.search_empty else R.string.empty_title,
        )
    }

    private fun buildDevices() {
        val content = findViewById<LinearLayout>(R.id.devices_content)
        content.addView(infoText(getString(R.string.available_profiles), true))
        DeviceProfiles.all.sortedBy { if (it.id == "k750i") 0 else 1 }.forEach { profile ->
            content.addView(infoText(profile.name, true))
            content.addView(infoText(profile.support(this)))
            content.addView(MaterialButton(this).apply {
                text = getString(R.string.import_for_phone, profile.name); isAllCaps = false
                setOnClickListener {
                    if (!busy) {
                        importDevice = profile.id
                        tabs.getTabAt(0)?.select()
                        status.text = profile.selectionHint(this@MainActivity)
                        importFirmware.launch(arrayOf("*/*"))
                    }
                }
            })
        }
        content.addView(infoText(getString(R.string.manufacturer_roadmap), true))
        val catalog = JSONArray(NativeBridge.nativeCatalog())
        for (index in 0 until catalog.length()) {
            val item = catalog.getJSONObject(index)
            val name = item.getString("name")
            val status = item.getString("status")
            val statusLabel = when (status) {
                "Experimental" -> getString(R.string.roadmap_experimental)
                "Planned" -> getString(R.string.roadmap_planned)
                else -> status
            }
            val detail = when {
                name == "Sony Ericsson" && status == "Experimental" -> getString(R.string.roadmap_k750i)
                status == "Planned" -> getString(R.string.roadmap_no_profile)
                else -> item.getString("detail")
            }
            content.addView(infoText(getString(R.string.roadmap_manufacturer, name, statusLabel), true))
            content.addView(infoText(detail))
        }
    }

    private fun buildSettings() {
        val content = findViewById<LinearLayout>(R.id.settings_content)
        languageButton = languageSetting()
        content.addView(languageButton)
        content.addView(infoText(getString(R.string.settings_display), true))
        content.addView(settingSwitch(getString(R.string.settings_show_fps), preferences.showFps) { preferences.showFps = it })
        content.addView(settingSwitch(getString(R.string.settings_sound), preferences.sound) { preferences.sound = it })
        content.addView(settingSwitch(getString(R.string.settings_fullscreen), preferences.fullscreen) { preferences.fullscreen = it })
        content.addView(settingSwitch(getString(R.string.settings_awake), preferences.keepAwake) { preferences.keepAwake = it })
        content.addView(infoText(getString(R.string.settings_phone_state), true))
        content.addView(settingSwitch(getString(R.string.settings_resume), preferences.resume) { preferences.resume = it })
        content.addView(infoText(getString(R.string.settings_resume_description)))
        content.addView(infoText(getString(R.string.settings_fps_description)))
        content.addView(infoText(getString(R.string.midi_soundfont), true))
        midiSoundFontState = infoText("")
        content.addView(midiSoundFontState)
        content.addView(infoText(getString(R.string.midi_soundfont_description)))
        selectMidiSoundFontButton = MaterialButton(this).apply {
            setText(R.string.midi_soundfont_choose)
            isAllCaps = false
            setOnClickListener { selectMidiSoundFont.launch(arrayOf("*/*")) }
        }
        content.addView(selectMidiSoundFontButton)
        resetMidiSoundFontButton = MaterialButton(this).apply {
            setText(R.string.midi_soundfont_reset)
            isAllCaps = false
            setOnClickListener { updateMidiSoundFont(null) }
        }
        content.addView(resetMidiSoundFontButton)
        refreshMidiSoundFontSetting()
    }

    private fun refreshMidiSoundFontSetting() {
        val installed = UserMidiSoundFont.path(this) != null
        midiSoundFontState.setText(if (installed) R.string.midi_soundfont_custom else R.string.midi_soundfont_default)
        selectMidiSoundFontButton.isEnabled = !busy
        resetMidiSoundFontButton.isEnabled = !busy && installed
    }

    private fun updateMidiSoundFont(uri: Uri?) {
        if (busy) return
        setBusy(true)
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    if (uri == null) {
                        UserMidiSoundFont.clear(this@MainActivity)
                    } else {
                        requireNotNull(contentResolver.openInputStream(uri)).use { input ->
                            UserMidiSoundFont.install(this@MainActivity, input)
                        }
                    }
                }
            }
            setBusy(false)
            if (result.isFailure) {
                showMessage(getString(if (uri == null) R.string.midi_soundfont_reset_failed else R.string.midi_soundfont_import_failed))
            }
        }
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
            .setTitle(getString(R.string.about_version, packageManager.getPackageInfo(packageName, 0).versionName))
            .setMessage(getString(R.string.about_description, notices))
            .setPositiveButton(R.string.close, null).show()
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
                    status.text = getString(R.string.library_error, error.localizedMessage ?: getString(R.string.unknown_error))
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
                    showMessage(getString(R.string.import_failed, error.localizedMessage ?: getString(R.string.unknown_error)))
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
                        ?: throw IllegalStateException(getString(R.string.firmware_missing))
                    library.attachOneNand(entry, uri)
                }
            }
            setBusy(false)
            result.fold(
                onSuccess = {
                    status.setText(R.string.nand_present)
                    refreshLibrary()
                },
                onFailure = { error -> showMessage(error.localizedMessage ?: getString(R.string.nand_use_failed)) },
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
                onFailure = { error -> showMessage(error.localizedMessage ?: getString(R.string.nand_remove_failed)) },
            )
        }
    }

    private fun chooseMemoryExport(entry: FirmwareEntry) {
        if (busy || pendingMemoryExport != null) return
        setBusy(true)
        status.setText(R.string.k750_memory_working)
        lifecycleScope.launch {
            val staged = File(cacheDir, "k750-export-${UUID.randomUUID()}.k750mem")
            try {
                withContext(Dispatchers.IO) { transferMemory(entry, staged, importing = false) }
                check(staged.isFile && staged.length() > 0) { getString(R.string.k750_memory_open_failed) }
                pendingMemoryExport = staged
                exportMemoryDocument.launch("phone-memory.k750mem")
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                pendingMemoryExport = null
                reportMemoryFailure(error)
            } finally {
                if (pendingMemoryExport != staged) staged.delete()
                setBusy(false)
            }
        }
    }

    private fun finishMemoryExport(uri: Uri?) {
        val staged = pendingMemoryExport
        pendingMemoryExport = null
        if (uri == null) {
            staged?.delete()
            refreshLibrary()
            return
        }
        setBusy(true)
        status.setText(R.string.k750_memory_working)
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO + NonCancellable) {
                try {
                    runCatching { writeDocumentExport(uri, staged) }
                } finally {
                    staged?.delete()
                }
            }
            setBusy(false)
            result.fold(
                onSuccess = { status.setText(R.string.k750_memory_exported) },
                onFailure = ::reportMemoryFailure,
            )
        }
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

    private fun finishMemoryImport(uri: Uri?) {
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
                        transferMemory(entry, staged, importing = true)
                    } finally {
                        staged.delete()
                    }
                }
            }
            setBusy(false)
            result.fold(
                onSuccess = { status.setText(R.string.k750_memory_imported) },
                onFailure = ::reportMemoryFailure,
            )
        }
    }

    private fun transferMemory(entry: FirmwareEntry, staged: File, importing: Boolean) {
        val failure = NativeBridge.nativeTransferInternalMemory(
            entry.selectionJson(), File(filesDir, "sessions").absolutePath,
            entry.memoryWorkspace?.absolutePath.orEmpty(), staged.absolutePath, importing,
        )
        check(failure.isEmpty()) { failure }
    }

    private fun reportMemoryFailure(error: Throwable) {
        val message = getString(R.string.k750_memory_failed, error.localizedMessage ?: "")
        status.text = message
        showMessage(message)
    }

    private fun runFirmware(entry: FirmwareEntry) {
        if (busy) return
        if (entry.device != "k750i") {
            launchFirmware(entry, null)
            return
        }
        val lastCard = preferences.phoneCard(entry.id)
        val cards = MemoryStickImages.list(MemoryStickImages.directory(filesDir)).sortedBy { it.name != lastCard }
        val choices = listOf(R.string.card_boot_without, R.string.card_boot_create, R.string.card_boot_import).map(::getString) +
            cards.map { MemoryStickImages.label(this, it) }
        MaterialAlertDialogBuilder(this).setTitle(R.string.card_boot_title)
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
        val labels = sizes.map { getString(if (it == 1024) R.string.size_gib else R.string.size_mib, if (it == 1024) 1 else it) }.toTypedArray()
        MaterialAlertDialogBuilder(this).setTitle(R.string.card_choose_size)
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
                    val directory = MemoryStickImages.directory(filesDir)
                    check(directory.isDirectory || directory.mkdirs()) { getString(R.string.card_folder_failed) }
                    val id = UUID.randomUUID()
                    val image = File(directory, "${if (source == null) "card" else "imported"}-$id.img")
                    val staged = File(directory, ".$id.tmp")
                    try {
                        if (source != null) {
                            val input = contentResolver.openInputStream(source) ?: error(getString(R.string.card_open_failed))
                            input.use {
                                staged.outputStream().use { output ->
                                    val buffer = ByteArray(64 * 1024)
                                    var total = 0L
                                    while (true) {
                                        val count = input.read(buffer)
                                        if (count == -1) break
                                        total += count
                                        check(total <= 1024L * 1024 * 1024) { getString(R.string.card_too_large) }
                                        output.write(buffer, 0, count)
                                    }
                                }
                            }
                        }
                        val request = JSONObject().put("image", staged.absolutePath)
                            .put("operation", if (source == null) "create" else "info")
                        if (source == null) request.put("size_mib", requireNotNull(sizeMib))
                        val response = JSONObject(NativeBridge.nativeCardRequest(request.toString()))
                        check(!response.has("error")) { response.optString("error") }
                        check(staged.renameTo(image)) { getString(R.string.card_save_failed) }
                    } finally {
                        staged.delete()
                    }
                    notifyMemoryStickChanged(this@MainActivity, image.name)
                    entry to image
                }
            }
            setBusy(false)
            result.onSuccess { (entry, image) -> launchFirmware(entry, image) }
                .onFailure { showMessage(it.localizedMessage ?: getString(R.string.card_prepare_failed)) }
        }
    }

    private fun launchFirmware(entry: FirmwareEntry, card: File?) {
        if (entry.device == "k750i" && card != null) {
            preferences.rememberPhoneCard(entry.id, card.name)
        }
        runEmulator.launch(
            Intent(this, EmulatorActivity::class.java)
                .putExtra(EmulatorActivity.EXTRA_SELECTION, entry.selectionJson())
                .putExtra(EmulatorActivity.EXTRA_PHONE_ID, entry.id)
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
                onFailure = { error -> showMessage(error.message ?: getString(R.string.firmware_remove_failed)) },
            )
        }
    }

    private fun setBusy(busy: Boolean) {
        this.busy = busy
        importButton.isEnabled = !busy
        languageButton.isEnabled = !busy
        refreshMidiSoundFontSetting()
        recycler.isEnabled = !busy
        recycler.alpha = if (busy) 0.55f else 1f
    }

    private fun showMessage(message: String) {
        Snackbar.make(requireNotNull(findViewById<View>(R.id.root)), message, Snackbar.LENGTH_LONG).show()
    }

    private companion object {
        const val STATE_NAND_ENTRY = "pendingNandEntryId"
        const val STATE_MEMORY_ENTRY = "pendingMemoryEntryId"
        const val STATE_MEMORY_EXPORT = "pendingMemoryExport"
        const val STATE_CARD_ENTRY = "pendingCardEntryId"
        const val MAX_MEMORY_FILE_BYTES = 65L * 1024 * 1024
        const val STATE_IMPORT_DEVICE = "importDevice"
    }
}
