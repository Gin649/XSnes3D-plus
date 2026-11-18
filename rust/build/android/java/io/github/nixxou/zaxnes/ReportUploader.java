package io.github.nixxou.zaxnes;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.Build;
import android.util.Base64;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;

/**
 * XSnes3D+: sends the play reports (see the Rust side, field_report.rs) to GitHub. Every finished
 * report in {@code <app files>/reports/*.txt} is uploaded to the {@code reports/} folder of the
 * player's own repository, then moved to {@code reports/sent}. It only runs when a token is set
 * (Options > Games and 3D profiles > Play reports); without one the reports just stay on the
 * device.
 */
final class ReportUploader {
    /** Where the reports go. */
    static final String REPOSITORY = "Gin649/zaxnes-plus-roms-";
    private static final String PREFERENCES = "xsnes3d";
    private static final String TOKEN = "report_token";
    /** The GitHub contents API takes files up to about 1 MB; longer reports keep their ends. */
    private static final int MAX_BYTES = 900 * 1024;
    private static final int HEAD_BYTES = 200 * 1024;
    private static final long PAUSE_MILLIS = 90_000;
    private static boolean started;

    private ReportUploader() {
    }

    static String token(Context c) {
        return prefs(c).getString(TOKEN, "");
    }

    static void setToken(Context c, String token) {
        prefs(c).edit().putString(TOKEN, token == null ? "" : token.trim()).apply();
    }

    private static SharedPreferences prefs(Context c) {
        return c.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE);
    }

    static File folder(Context c) {
        File root = c.getExternalFilesDir(null);
        return root == null ? null : new File(root, "reports");
    }

    /** The reports waiting to be sent. */
    static int waiting(Context c) {
        File dir = folder(c);
        File[] files = dir == null ? null : dir.listFiles();
        int count = 0;
        if (files != null) {
            for (File f : files) {
                if (f.getName().endsWith(".txt")) count++;
            }
        }
        File[] moments = dir == null ? null : new File(dir, "moments").listFiles();
        if (moments != null) {
            for (File f : moments) {
                if (f.isDirectory() && !f.getName().endsWith(".part")) count++;
            }
        }
        return count;
    }

    /** Starts the background sender once per process: now, then every 90 seconds. */
    static synchronized void start(final Context context) {
        if (started) return;
        started = true;
        final Context c = context.getApplicationContext();
        Thread thread = new Thread(new Runnable() {
            @Override
            public void run() {
                while (true) {
                    try {
                        sendAll(c);
                    } catch (Throwable ignored) {
                        // No network, GitHub down: the next round tries again.
                    }
                    try {
                        Thread.sleep(PAUSE_MILLIS);
                    } catch (InterruptedException e) {
                        return;
                    }
                }
            }
        }, "XSnes3D+ reports");
        thread.setDaemon(true);
        thread.start();
    }

    /** Sends every finished report. Returns how many went, or throws the first failure. */
    static int sendAll(Context c) throws Exception {
        String token = token(c);
        File dir = folder(c);
        if (token.isEmpty() || dir == null) return 0;
        File[] files = dir.listFiles();
        if (files == null) return 0;
        File sent = new File(dir, "sent");
        sent.mkdirs();
        int count = 0;
        for (File f : files) {
            if (!f.isFile() || !f.getName().endsWith(".txt")) continue;
            upload(token, f, "reports/" + device() + "/" + f.getName(), true);
            f.renameTo(new File(sent, f.getName()));
            count++;
        }
        // The moments ("Report this moment"): one folder each, sent file by file. A folder still
        // ending in .part is being written.
        File moments = new File(dir, "moments");
        File[] folders = moments.listFiles();
        if (folders != null) {
            File sentMoments = new File(sent, "moments");
            sentMoments.mkdirs();
            for (File folder : folders) {
                if (!folder.isDirectory() || folder.getName().endsWith(".part")) continue;
                File[] parts = folder.listFiles();
                if (parts == null) continue;
                for (File part : parts) {
                    if (!part.isFile()) continue;
                    upload(token, part, "reports/" + device() + "/moments/" + folder.getName()
                        + "/" + part.getName(), false);
                }
                folder.renameTo(new File(sentMoments, folder.getName()));
                count++;
            }
        }
        return count;
    }

    private static String device() {
        return (Build.MANUFACTURER + "-" + Build.MODEL).replaceAll("[^A-Za-z0-9]+", "-");
    }

    private static void upload(String token, File file, String path, boolean trim)
        throws Exception {
        byte[] text = trim ? trimmed(read(file)) : read(file);
        String body = "{\"message\":\"Play report " + file.getName() + "\",\"content\":\""
            + Base64.encodeToString(text, Base64.NO_WRAP) + "\"}";
        URL url = new URL("https://api.github.com/repos/" + REPOSITORY + "/contents/" + path);
        HttpURLConnection http = (HttpURLConnection) url.openConnection();
        http.setRequestMethod("PUT");
        http.setConnectTimeout(20_000);
        http.setReadTimeout(60_000);
        http.setDoOutput(true);
        http.setRequestProperty("Authorization", "Bearer " + token);
        http.setRequestProperty("Accept", "application/vnd.github+json");
        http.setRequestProperty("Content-Type", "application/json");
        http.setRequestProperty("User-Agent", "XSnes3D-plus");
        try (OutputStream out = http.getOutputStream()) {
            out.write(body.getBytes(StandardCharsets.UTF_8));
        }
        int code = http.getResponseCode();
        http.disconnect();
        // 422: a report of that name is there already (sent before a crash moved it).
        if (code != 200 && code != 201 && code != 422) {
            throw new Exception("GitHub answered " + code
                + (code == 401 || code == 403 || code == 404 ? " (check the token)" : ""));
        }
    }

    private static byte[] read(File file) throws Exception {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        try (InputStream in = new FileInputStream(file)) {
            byte[] buffer = new byte[64 * 1024];
            int n;
            while ((n = in.read(buffer)) > 0) out.write(buffer, 0, n);
        }
        return out.toByteArray();
    }

    /** A report too long for one upload keeps its start (the game and device) and its end. */
    private static byte[] trimmed(byte[] text) {
        if (text.length <= MAX_BYTES) return text;
        byte[] cut = "\n== ... (the middle of a long report was left out) ...\n"
            .getBytes(StandardCharsets.UTF_8);
        int tail = MAX_BYTES - HEAD_BYTES - cut.length;
        byte[] out = new byte[MAX_BYTES];
        System.arraycopy(text, 0, out, 0, HEAD_BYTES);
        System.arraycopy(cut, 0, out, HEAD_BYTES, cut.length);
        System.arraycopy(text, text.length - tail, out, HEAD_BYTES + cut.length, tail);
        return out;
    }
}
