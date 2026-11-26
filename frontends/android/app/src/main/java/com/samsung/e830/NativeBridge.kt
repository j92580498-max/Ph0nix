package com.samsung.e830

internal object NativeBridge {
    init {
        System.loadLibrary("e830_android_jni")
    }

    external fun nativeValidate(cla: String, eeprom: String, tfs: String, onenand: String): String
    external fun nativeStart(cla: String, eeprom: String, tfs: String, onenand: String): Long
    external fun nativeProfiles(): String
    external fun nativeCatalog(): String
    external fun nativeConfigure(engine: String, dataDirectory: String, resume: Boolean)
    external fun nativeSetKeys(handle: Long, mask: Int)
    external fun nativeKeyBindings(device: String): String
    external fun nativeValidateSelection(selection: String): String
    external fun nativeCreateInternalMemory(selection: String, workspace: String): String
    external fun nativeTransferInternalMemory(
        selection: String, dataDirectory: String, workspace: String, file: String, importing: Boolean,
    ): String
    external fun nativeStartSelection(selection: String, workspace: String): Long
    external fun nativePollStatus(handle: Long): String
    external fun nativeTakeFrame(handle: Long): ByteArray?
    external fun nativeTakeAudio(handle: Long): ByteArray?
    external fun nativeDiagnostics(handle: Long): String
    external fun nativeRecordPresentations(handle: Long, count: Int)
    external fun nativeRecordRender(handle: Long, nanos: Long)
    external fun nativeSetDeveloperMode(handle: Long, enabled: Boolean)
    external fun nativeLog(handle: Long, level: String, component: String, message: String)
    external fun nativeFiles(handle: Long): String
    external fun nativeFileRequest(handle: Long, request: String): String
    external fun nativeBluetoothSenderOpen(path: String): Long
    external fun nativeBluetoothSenderNext(handle: Long): ByteArray?
    external fun nativeBluetoothSenderResponse(handle: Long, packet: ByteArray): String
    external fun nativeBluetoothSenderProgress(handle: Long): String
    external fun nativeBluetoothSenderClose(handle: Long)
    external fun nativeBluetoothReceiverOpen(directory: String): Long
    external fun nativeBluetoothReceiverPacket(handle: Long, packet: ByteArray): ByteArray?
    external fun nativeBluetoothReceiverTake(handle: Long): String
    external fun nativeBluetoothReceiverClose(handle: Long)
    external fun nativeCardRequest(request: String): String
    external fun nativeIsRunning(handle: Long): Boolean
    external fun nativeStop(handle: Long)
    external fun nativeDestroy(handle: Long)
}
