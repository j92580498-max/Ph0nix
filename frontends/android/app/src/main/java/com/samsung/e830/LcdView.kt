package com.samsung.e830

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.RectF
import android.util.AttributeSet
import android.view.View
import android.view.Choreographer
import kotlin.math.min

internal class LcdView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
    defStyleAttr: Int = 0,
) : View(context, attrs, defStyleAttr) {
    private var frame: Bitmap? = null
    private var colors = IntArray(0)
    private var running = false
    private var nextFrameNs = 0L
    private var windowStartNs = 0L
    private var windowFrames = 0
    var presentationFps = 0f
        private set
    var presentationCount = 0L
        private set
    var lastRenderNanos = 0L
        private set
    var showFps = true
    var onPresentationFrame: (() -> Unit)? = null
    private var fpsText = "0.0 FPS"
    private val destination = RectF()
    private val overlayPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.WHITE
        textSize = 12 * resources.displayMetrics.scaledDensity
        setShadowLayer(2f, 1f, 1f, Color.BLACK)
    }
    private val clock = Choreographer.getInstance()
    private val frameCallback = object : Choreographer.FrameCallback {
        override fun doFrame(timeNs: Long) {
            if (!running) return
            if (timeNs + 1_000_000L >= nextFrameNs) {
                // Preserve phase across slightly uneven 60/90/120 Hz callbacks.
                if (nextFrameNs == 0L) nextFrameNs = timeNs
                do { nextFrameNs += 16_666_667L } while (nextFrameNs <= timeNs + 1_000_000L)
                onPresentationFrame?.invoke()
                invalidate()
            }
            clock.postFrameCallback(this)
        }
    }
    private val paint = Paint().apply {
        isAntiAlias = false
        isFilterBitmap = false
    }

    fun setFrame(packet: ByteArray) {
        if (packet.size < 8) return
        val header = java.nio.ByteBuffer.wrap(packet, 0, 8).order(java.nio.ByteOrder.LITTLE_ENDIAN)
        val width = header.int
        val height = header.int
        val pixels = width.toLong() * height
        if (width <= 0 || height <= 0 || pixels > 4_000_000L) return
        val rgb565 = pixels * 2 == packet.size.toLong() - 8
        if (!rgb565 && pixels * 3 != packet.size.toLong() - 8) return
        val config = if (rgb565) Bitmap.Config.RGB_565 else Bitmap.Config.ARGB_8888
        if (frame?.width != width || frame?.height != height || frame?.config != config) {
            frame?.recycle()
            frame = Bitmap.createBitmap(width, height, config)
            colors = if (rgb565) IntArray(0) else IntArray(pixels.toInt())
        }
        if (rgb565) {
            frame!!.copyPixelsFromBuffer(java.nio.ByteBuffer.wrap(packet, 8, packet.size - 8))
        } else {
            var source = 8
            for (index in colors.indices) {
                val red = packet[source++].toInt() and 0xff
                val green = packet[source++].toInt() and 0xff
                val blue = packet[source++].toInt() and 0xff
                colors[index] = (0xff shl 24) or (red shl 16) or (green shl 8) or blue
            }
            frame!!.setPixels(colors, 0, width, 0, 0, width, height)
        }
        if (!running) invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        val started = System.nanoTime()
        super.onDraw(canvas)
        canvas.drawColor(Color.BLACK)
        val bitmap = frame ?: return
        if (width <= 0 || height <= 0) return
        val scale = min(width.toFloat() / bitmap.width, height.toFloat() / bitmap.height)
            .coerceAtLeast(0.1f)
        val displayWidth = bitmap.width * scale
        val displayHeight = bitmap.height * scale
        val left = (width - displayWidth) / 2f
        val top = (height - displayHeight) / 2f
        destination.set(left, top, left + displayWidth, top + displayHeight)
        canvas.drawBitmap(bitmap, null, destination, paint)
        if (running) {
            presentationCount++
            val now = System.nanoTime()
            if (windowStartNs == 0L) windowStartNs = now
            windowFrames++
            val elapsed = now - windowStartNs
            if (elapsed >= 1_000_000_000L) {
                presentationFps = windowFrames * 1_000_000_000f / elapsed
                fpsText = String.format(java.util.Locale.ROOT, "%.1f FPS", presentationFps)
                windowFrames = 0; windowStartNs = now
            }
        }
        if (showFps) canvas.drawText(fpsText, 8 * resources.displayMetrics.density,
            overlayPaint.textSize + 8 * resources.displayMetrics.density, overlayPaint)
        lastRenderNanos = System.nanoTime() - started
    }

    fun releaseFrame() {
        setRunning(false)
        onPresentationFrame = null
        frame?.recycle()
        frame = null
        invalidate()
    }

    fun setRunning(enabled: Boolean) {
        if (running == enabled) return
        running = enabled
        clock.removeFrameCallback(frameCallback)
        if (enabled) {
            nextFrameNs = 0; windowStartNs = 0; windowFrames = 0
            presentationFps = 0f; fpsText = "0.0 FPS"
            clock.postFrameCallback(frameCallback)
        }
    }
}
