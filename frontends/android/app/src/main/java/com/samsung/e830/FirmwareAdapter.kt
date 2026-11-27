package com.samsung.e830

import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.ImageButton
import android.widget.PopupMenu
import android.widget.TextView
import androidx.recyclerview.widget.RecyclerView

internal class FirmwareAdapter(
    private val onRun: (FirmwareEntry) -> Unit,
    private val onRename: (FirmwareEntry) -> Unit,
    private val onSelectOneNand: (FirmwareEntry) -> Unit,
    private val onUseErasedFlash: (FirmwareEntry) -> Unit,
    private val onExportMemory: (FirmwareEntry) -> Unit,
    private val onImportMemory: (FirmwareEntry) -> Unit,
    private val onRemove: (FirmwareEntry) -> Unit,
) : RecyclerView.Adapter<FirmwareAdapter.ViewHolder>() {
    private var items: List<FirmwareEntry> = emptyList()
    var grid = true
        set(value) { field = value; notifyDataSetChanged() }

    fun submit(entries: List<FirmwareEntry>) {
        items = entries
        notifyDataSetChanged()
    }

    override fun onCreateViewHolder(parent: ViewGroup, viewType: Int): ViewHolder =
        ViewHolder(LayoutInflater.from(parent.context).inflate(if (viewType == 0) R.layout.item_firmware else R.layout.item_firmware_list, parent, false))

    override fun getItemViewType(position: Int): Int = if (grid) 0 else 1

    override fun onBindViewHolder(holder: ViewHolder, position: Int) = holder.bind(items[position])

    override fun getItemCount(): Int = items.size

    inner class ViewHolder(view: View) : RecyclerView.ViewHolder(view) {
        private val title = requireNotNull(view.findViewById<TextView>(R.id.firmware_title))
        private val subtitle = requireNotNull(view.findViewById<TextView>(R.id.firmware_subtitle))
        private val more = requireNotNull(view.findViewById<ImageButton>(R.id.btn_more))

        fun bind(entry: FirmwareEntry) {
            title.text = entry.name
            val profile = entry.profile
            subtitle.text = "${profile.name}\n${profile.displayWidth} × ${profile.displayHeight} LCD"
            itemView.setOnClickListener { onRun(entry) }
            requireNotNull(itemView.findViewById<View>(R.id.btn_run)).setOnClickListener { onRun(entry) }
            more.setOnClickListener { showMenu(it, entry) }
            itemView.setOnLongClickListener { showMenu(more, entry); true }
        }

        private fun showMenu(anchor: View, entry: FirmwareEntry) {
            val popup = PopupMenu(anchor.context, anchor)
            popup.menu.add(0, ACTION_RUN, 0, R.string.run_firmware)
            popup.menu.add(0, ACTION_RENAME, 0, R.string.rename_phone)
            if (entry.profile.hasNand) popup.menu.add(0, ACTION_SELECT_NAND, 0, R.string.nand_select)
            if (entry.onenand != null) {
                popup.menu.add(0, ACTION_USE_ERASED, 1, R.string.nand_auto)
            }
            if (entry.device == "k750i") {
                popup.menu.add(0, ACTION_EXPORT_MEMORY, 2, R.string.k750_memory_export)
                popup.menu.add(0, ACTION_IMPORT_MEMORY, 2, R.string.k750_memory_import)
            }
            popup.menu.add(0, ACTION_REMOVE, 3, R.string.remove_firmware)
            popup.setOnMenuItemClickListener { item ->
                when (item.itemId) {
                    ACTION_RUN -> onRun(entry)
                    ACTION_RENAME -> onRename(entry)
                    ACTION_SELECT_NAND -> onSelectOneNand(entry)
                    ACTION_USE_ERASED -> onUseErasedFlash(entry)
                    ACTION_EXPORT_MEMORY -> onExportMemory(entry)
                    ACTION_IMPORT_MEMORY -> onImportMemory(entry)
                    ACTION_REMOVE -> onRemove(entry)
                }
                true
            }
            popup.show()
        }
    }

    private companion object {
        const val ACTION_SELECT_NAND = 1
        const val ACTION_USE_ERASED = 2
        const val ACTION_REMOVE = 3
        const val ACTION_RUN = 4
        const val ACTION_EXPORT_MEMORY = 5
        const val ACTION_IMPORT_MEMORY = 6
        const val ACTION_RENAME = 7
    }
}
