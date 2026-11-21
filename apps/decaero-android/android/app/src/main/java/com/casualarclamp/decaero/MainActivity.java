// made by CasualArclamp
package com.casualarclamp.decaero;

import android.app.NativeActivity;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.hardware.usb.UsbDevice;
import android.hardware.usb.UsbDeviceConnection;
import android.hardware.usb.UsbManager;
import android.os.Build;
import android.os.Bundle;
import android.os.ParcelFileDescriptor;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/**
 * The Android side of DecAero. The program itself is the Rust library
 * (apps/decaero-android/src/lib.rs), which NativeActivity loads and runs;
 * this class adds the three things only Java can ask Android for: a USB
 * radio (with its permission dialog), a file from the system's chooser, and
 * a screen that stays on and has no bars.
 *
 * Receive only: a radio is opened, never keyed.
 */
public class MainActivity extends NativeActivity {
    private static final String USB_PERMISSION = "com.casualarclamp.decaero.USB_PERMISSION";
    private static final int PICK_FILE = 1;

    /** The radio in use. Kept so its descriptor stays open for the Rust side. */
    private UsbDeviceConnection connection;
    /** A chosen file's descriptor; -1 while choosing, -2 when nothing was chosen. */
    private volatile int pickedFd = -1;

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        // A receiver is watched, not touched: do not let the screen sleep.
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        hideBars();
    }

    @Override
    public void onWindowFocusChanged(boolean focused) {
        super.onWindowFocusChanged(focused);
        if (focused) {
            hideBars();
        }
    }

    /** The whole screen for the band: the bars come back with a swipe. */
    @SuppressWarnings("deprecation")
    private void hideBars() {
        if (Build.VERSION.SDK_INT >= 30) {
            WindowInsetsController bars = getWindow().getInsetsController();
            if (bars != null) {
                bars.hide(WindowInsets.Type.systemBars());
                bars.setSystemBarsBehavior(
                        WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
            }
        } else {
            getWindow().getDecorView().setSystemUiVisibility(
                    View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                            | View.SYSTEM_UI_FLAG_FULLSCREEN
                            | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                            | View.SYSTEM_UI_FLAG_LAYOUT_STABLE);
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        // Started again by a radio being plugged in: nothing to do, the
        // program is already running.
        super.onNewIntent(intent);
    }

    /** The command line, for trying things: am start ... --es args "...". */
    public String startArgs() {
        String args = getIntent().getStringExtra("args");
        return args == null ? "" : args;
    }

    /**
     * Open a USB device with one of the given ids ({vendor, product, vendor,
     * product ...}) and return its file descriptor, asking the user first if
     * need be. Called from a Rust thread, never the one that draws: it waits
     * for the answer.
     *
     * Returns -1 if no such device is plugged in, -2 if permission was
     * refused, -3 if Android could not open it, -4 if the question went
     * unanswered for a minute.
     */
    public int openUsb(int[] ids) {
        UsbManager usb = (UsbManager) getSystemService(Context.USB_SERVICE);
        if (usb == null) {
            return -3;
        }
        UsbDevice radio = null;
        for (UsbDevice device : usb.getDeviceList().values()) {
            for (int i = 0; i + 1 < ids.length; i += 2) {
                if (device.getVendorId() == ids[i] && device.getProductId() == ids[i + 1]) {
                    radio = device;
                }
            }
        }
        if (radio == null) {
            return -1;
        }
        if (!usb.hasPermission(radio)) {
            final CountDownLatch answered = new CountDownLatch(1);
            BroadcastReceiver receiver = new BroadcastReceiver() {
                @Override
                public void onReceive(Context context, Intent intent) {
                    answered.countDown();
                }
            };
            IntentFilter filter = new IntentFilter(USB_PERMISSION);
            if (Build.VERSION.SDK_INT >= 33) {
                registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED);
            } else {
                registerReceiver(receiver, filter);
            }
            // Android fills in the answer, so the intent must be changeable,
            // and since Android 14 such an intent must name its package.
            Intent answer = new Intent(USB_PERMISSION);
            answer.setPackage(getPackageName());
            int flags = Build.VERSION.SDK_INT >= 31 ? PendingIntent.FLAG_MUTABLE : 0;
            usb.requestPermission(radio, PendingIntent.getBroadcast(this, 0, answer, flags));
            boolean inTime = false;
            try {
                inTime = answered.await(60, TimeUnit.SECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            unregisterReceiver(receiver);
            if (!usb.hasPermission(radio)) {
                return inTime ? -2 : -4;
            }
        }
        synchronized (this) {
            // The Rust side has its own copy of the last descriptor.
            if (connection != null) {
                connection.close();
            }
            connection = usb.openDevice(radio);
            return connection == null ? -3 : connection.getFileDescriptor();
        }
    }

    /** Show the system's file chooser; takePickedFd() has the answer later. */
    public void pickFile() {
        pickedFd = -1;
        runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            // Recordings are WAV, but file managers disagree on its type.
            intent.setType("*/*");
            try {
                startActivityForResult(intent, PICK_FILE);
            } catch (RuntimeException e) {
                pickedFd = -2;
            }
        });
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != PICK_FILE) {
            return;
        }
        int fd = -2;
        if (result == RESULT_OK && data != null && data.getData() != null) {
            try (ParcelFileDescriptor file =
                    getContentResolver().openFileDescriptor(data.getData(), "r")) {
                if (file != null) {
                    // Ours from here on: the Rust side closes it.
                    fd = file.detachFd();
                }
            } catch (Exception e) {
                fd = -2;
            }
        }
        pickedFd = fd;
    }

    /**
     * The chosen file's descriptor, once: -1 while the chooser is still
     * open, -2 if it was closed without a file.
     */
    public int takePickedFd() {
        int fd = pickedFd;
        if (fd != -1) {
            pickedFd = -1;
        }
        return fd;
    }

    @Override
    protected void onDestroy() {
        synchronized (this) {
            if (connection != null) {
                connection.close();
                connection = null;
            }
        }
        super.onDestroy();
    }
}
