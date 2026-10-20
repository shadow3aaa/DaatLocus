package io.shadow3aaa.daatlocus;

import android.security.NetworkSecurityPolicy;

// Exercise the final APK's policy, including resource overlays and R8 packaging.
final class NetworkSecurityTest {
    static void check() {
        NetworkSecurityPolicy policy = NetworkSecurityPolicy.getInstance();
        if (policy.isCleartextTrafficPermitted()) {
            throw new AssertionError("APK permits cleartext traffic by default");
        }
        for (String host : new String[] {
            "c.pki.goog", "yr2.c.lencr.org", "127.0.0.1", "localhost",
            "tauri.localhost", "ipc.localhost", "dl-resource.localhost"
        }) {
            if (!policy.isCleartextTrafficPermitted(host)) {
                throw new AssertionError("APK lost the CRL/local network allowance for " + host);
            }
        }
        for (String host : new String[] {"example.org", "api.example.test", "sub.localhost"}) {
            if (policy.isCleartextTrafficPermitted(host)) {
                throw new AssertionError("APK unexpectedly allows cleartext to " + host);
            }
        }
    }
}
