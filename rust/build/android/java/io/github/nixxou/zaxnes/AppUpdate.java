package io.github.nixxou.zaxnes;

import android.app.Activity;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInstaller;
import android.os.Build;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;

/**
 * XSnes3D+: finds the newest XSnes3D+ release on GitHub, downloads its APK and hands it to
 * Android's installer, which asks the player to confirm. Every build is signed with the same
 * key, so the update installs over the app and keeps its data. Used by ProfilesActivity
 * (Options > Check for updates). No lambdas: the APK is compiled without LambdaMetafactory.
 */
final class AppUpdate {
    private AppUpdate() {}

    /**
     * The releases, newest first. Not ".../releases/latest": that one skips pre-releases, and
     * every test build so far is a pre-release, so it answered 404.
     */
    static final String RELEASES =
        "https://api.github.com/repos/Gin649/XSnes3D-plus/releases?per_page=30";

    /** The action of the intent Android's installer sends back when it has news. */
    static final String STATUS_ACTION = "io.github.gin649.xsnes3d.UPDATE_STATUS";

    /** A release: its version ("1.0.3"), notes and APK. */
    static final class Release {
        String version;
        String notes;
        String apkUrl;
        long apkSize;
    }

    /** Something to tell while the APK downloads: the share done, 0 to 100. */
    interface Progress {
        void at(int percent);
    }

    /** The version installed ("1.0.2"), or "" when Android doesn't say. */
    static String installed(Context context) {
        try {
            String name = context.getPackageManager()
                .getPackageInfo(context.getPackageName(), 0).versionName;
            return name == null ? "" : name;
        } catch (Exception e) {
            return "";
        }
    }

    /** The newest release. Throws with a readable message when it can't be read. */
    static Release latest() throws Exception {
        HttpURLConnection c = (HttpURLConnection) new URL(RELEASES).openConnection();
        c.setRequestProperty("Accept", "application/vnd.github+json");
        c.setRequestProperty("User-Agent", "XSnes3D+");
        c.setConnectTimeout(15000);
        c.setReadTimeout(15000);
        int code = c.getResponseCode();
        if (code != 200) throw new Exception("GitHub answered " + code + ". Try again later.");
        String text;
        try (InputStream in = c.getInputStream()) {
            ByteArrayOutputStream bytes = new ByteArrayOutputStream();
            byte[] buffer = new byte[16 * 1024];
            int n;
            while ((n = in.read(buffer)) > 0) bytes.write(buffer, 0, n);
            text = bytes.toString("UTF-8");
        } finally {
            c.disconnect();
        }
        // The highest version among the app's releases ("v" and a number: not the build tools'
        // "dev-vendor") that have an APK, drafts left out.
        JSONArray releases = new JSONArray(text);
        Release best = null;
        for (int i = 0; i < releases.length(); i++) {
            Release r = release(releases.getJSONObject(i));
            if (r != null && (best == null || newer(r.version, best.version))) best = r;
        }
        if (best == null) throw new Exception("No release has an APK yet.");
        return best;
    }

    /** One release of the list, or null when it is a draft, not the app's, or has no APK. */
    private static Release release(JSONObject json) throws Exception {
        if (json.optBoolean("draft", false)) return null;
        String tag = json.optString("tag_name", "");
        if (tag.length() < 2 || tag.charAt(0) != 'v' || !Character.isDigit(tag.charAt(1))) return null;
        Release r = new Release();
        r.version = tag.substring(1);
        r.notes = json.optString("body", "");
        JSONArray assets = json.optJSONArray("assets");
        if (assets != null) {
            for (int i = 0; i < assets.length(); i++) {
                JSONObject a = assets.getJSONObject(i);
                if (a.optString("name", "").toLowerCase().endsWith(".apk")) {
                    r.apkUrl = a.optString("browser_download_url", null);
                    r.apkSize = a.optLong("size", 0);
                    break;
                }
            }
        }
        return r.apkUrl == null ? null : r;
    }

    /** True when version {@code a} ("1.0.10") is newer than {@code b} ("1.0.9"). */
    static boolean newer(String a, String b) {
        String[] x = a.split("\\.");
        String[] y = b.split("\\.");
        for (int i = 0; i < Math.max(x.length, y.length); i++) {
            int p = i < x.length ? number(x[i]) : 0;
            int q = i < y.length ? number(y[i]) : 0;
            if (p != q) return p > q;
        }
        return false;
    }

    private static int number(String part) {
        StringBuilder digits = new StringBuilder();
        for (char ch : part.toCharArray()) {
            if (!Character.isDigit(ch)) break;
            digits.append(ch);
        }
        try {
            return digits.length() == 0 ? 0 : Integer.parseInt(digits.toString());
        } catch (NumberFormatException e) {
            return 0;
        }
    }

    /** Downloads the release's APK to {@code out}, telling {@code progress} as it goes. */
    static void download(Release r, File out, Progress progress) throws Exception {
        File folder = out.getParentFile();
        if (folder != null) folder.mkdirs();
        HttpURLConnection c = (HttpURLConnection) new URL(r.apkUrl).openConnection();
        c.setRequestProperty("User-Agent", "XSnes3D+");
        c.setInstanceFollowRedirects(true);
        c.setConnectTimeout(15000);
        c.setReadTimeout(30000);
        int code = c.getResponseCode();
        if (code != 200) throw new Exception("The download answered " + code + ". Try again later.");
        long total = c.getContentLengthLong() > 0 ? c.getContentLengthLong() : r.apkSize;
        File part = new File(out.getPath() + ".part");
        try (InputStream in = c.getInputStream(); OutputStream o = new FileOutputStream(part)) {
            byte[] buffer = new byte[64 * 1024];
            long done = 0;
            int last = -1;
            int n;
            while ((n = in.read(buffer)) > 0) {
                o.write(buffer, 0, n);
                done += n;
                int percent = total > 0 ? (int) (done * 100 / total) : 0;
                if (percent != last) {
                    last = percent;
                    progress.at(percent);
                }
            }
        } finally {
            c.disconnect();
        }
        out.delete();
        if (!part.renameTo(out)) throw new Exception("Couldn't save the download.");
    }

    /**
     * Hands the APK at {@code apk} to Android's installer. Android then shows its own "update
     * this app?" screen; its answer comes back to ProfilesActivity as a {@link #STATUS_ACTION}
     * intent.
     */
    static void install(Activity activity, File apk) throws Exception {
        PackageInstaller installer = activity.getPackageManager().getPackageInstaller();
        PackageInstaller.SessionParams params =
            new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
        params.setAppPackageName(activity.getPackageName());
        int id = installer.createSession(params);
        PackageInstaller.Session session = installer.openSession(id);
        try {
            try (InputStream in = new FileInputStream(apk);
                 OutputStream o = session.openWrite("xsnes3d-plus.apk", 0, apk.length())) {
                byte[] buffer = new byte[64 * 1024];
                int n;
                while ((n = in.read(buffer)) > 0) o.write(buffer, 0, n);
                session.fsync(o);
            }
            Intent back = new Intent(activity, ProfilesActivity.class);
            back.setAction(STATUS_ACTION);
            int flags = PendingIntent.FLAG_UPDATE_CURRENT;
            // Android 12 and later want the mutability said; the installer fills the intent in.
            if (Build.VERSION.SDK_INT >= 31) flags |= PendingIntent.FLAG_MUTABLE;
            PendingIntent pending = PendingIntent.getActivity(activity, id, back, flags);
            session.commit(pending.getIntentSender());
        } catch (Exception e) {
            session.abandon();
            throw e;
        } finally {
            session.close();
        }
    }
}
