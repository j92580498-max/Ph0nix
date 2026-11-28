package com.samsung.e830

import androidx.appcompat.app.AppCompatActivity
import androidx.appcompat.app.AppCompatDelegate
import androidx.core.os.LocaleListCompat
import com.google.android.material.button.MaterialButton
import com.google.android.material.dialog.MaterialAlertDialogBuilder

internal fun AppCompatActivity.languageSetting(): MaterialButton {
    val choices = listOf("" to R.string.language_system, "en" to R.string.language_english, "ru" to R.string.language_russian)
    val current = AppCompatDelegate.getApplicationLocales()[0]
    val selected = choices.indexOfFirst { it.first == current?.language.orEmpty() }
    val labels = choices.map { getString(it.second) }.toTypedArray()
    val label = labels.getOrNull(selected) ?: current?.let { it.getDisplayName(it) }.orEmpty()
    return MaterialButton(this, null, com.google.android.material.R.attr.materialButtonOutlinedStyle).apply {
        isAllCaps = false
        text = getString(R.string.language_setting, label)
        setOnClickListener {
            MaterialAlertDialogBuilder(this@languageSetting)
                .setTitle(R.string.language)
                .setSingleChoiceItems(labels, selected) { dialog, index ->
                    dialog.dismiss()
                    AppCompatDelegate.setApplicationLocales(LocaleListCompat.forLanguageTags(choices[index].first))
                }
                .setNegativeButton(R.string.cancel, null)
                .show()
        }
    }
}
