import type { CapacitorConfig } from "@capacitor/cli";
import { KeyboardResize } from "@capacitor/keyboard";

// Cortex native shell. The web app is the mobile SPA in ../ (Vite → ../dist);
// this package only wraps it. Native projects (android/, ios/) are generated in
// CI by `cap add` and are gitignored; scripts/mobile/patch-native.sh applies the
// bits `cap add` does not know about (deep-link scheme, LAN http, usage strings).
const config: CapacitorConfig = {
  appId: "com.networkbound.cortex",
  appName: "Cortex",
  webDir: "../dist",
  // Android: serve the bundle from https://localhost so secure-context APIs
  // (clipboard, service worker, crypto.subtle) work like they do in a browser.
  server: {
    androidScheme: "https",
  },
  android: {
    // The phone talks to a desktop Cortex over Tailscale/LAN, often plain http
    // (100.x.y.z:8788). Cleartext is allowed via res/xml/network_security_config.xml
    // written by patch-native.sh; see docs/MOBILE.md "Limitations" for the risk.
    allowMixedContent: false,
  },
  ios: {
    contentInset: "automatic",
    // Native scrolling feel; the SPA sets viewport-fit=cover itself.
    scrollEnabled: true,
  },
  plugins: {
    SplashScreen: {
      launchAutoHide: true,
      launchShowDuration: 600,
      launchFadeOutDuration: 200,
      backgroundColor: "#0a0a0b",
      showSpinner: false,
      androidScaleType: "CENTER_CROP",
      splashFullScreen: false,
      splashImmersive: false,
    },
    Keyboard: {
      // Resize the WebView itself (not just the body) so the composer stays
      // above the keyboard on iOS; Android handles this via adjustResize.
      resize: KeyboardResize.Native,
      resizeOnFullScreen: true,
    },
    StatusBar: {
      style: "DARK",
      backgroundColor: "#0a0a0b",
      overlaysWebView: false,
    },
  },
};

export default config;
