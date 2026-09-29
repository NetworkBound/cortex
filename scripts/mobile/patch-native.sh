#!/usr/bin/env bash
# Apply the Cortex-specific tweaks that `npx cap add <platform>` does not know
# about. Idempotent: safe to re-run after every `cap add` (the native projects
# are gitignored and regenerated in CI).
#
# Usage:  bash scripts/mobile/patch-native.sh android|ios [version]
#   version defaults to the root package.json version (e.g. 3.4.0)
#
# android:
#   - cortex:// deep-link intent-filter on MainActivity
#   - res/xml/network_security_config.xml allowing cleartext http (plain http to
#     a LAN/tailnet Cortex — see docs/MOBILE.md "Limitations"); the manifest
#     points at it via android:networkSecurityConfig
#   - CAMERA permission + ML Kit code-scanner module hint (QR pairing)
#   - versionName / versionCode from the Cortex version
# ios:
#   - CFBundleURLTypes for cortex://
#   - NSCameraUsageDescription, NSLocalNetworkUsageDescription
#   - NSAppTransportSecurity: local networking + arbitrary loads (plain http)
#   - ITSAppUsesNonExemptEncryption=false (no export-compliance prompt)
#   - PrivacyInfo.xcprivacy: no tracking; UserDefaults reason CA92.1 (Preferences)
#   - MARKETING_VERSION / CURRENT_PROJECT_VERSION in the Xcode project
#
# Only bash + python3 are required (no PlistBuddy/xmlstarlet), so it also runs
# on ubuntu-latest for the Android job.
set -euo pipefail

PLATFORM="${1:-}"
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
NATIVE_DIR="$REPO_ROOT/mobile/native"

VERSION="${2:-$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$REPO_ROOT/package.json")}"
# 3.4.0 -> 30400 ; monotonically increasing per semver so store uploads accept it.
VERSION_CODE=$(python3 -c '
import re,sys
m=re.match(r"^(\d+)\.(\d+)\.(\d+)", sys.argv[1])
a,b,c=(int(x) for x in m.groups()) if m else (0,0,1)
print(a*10000+b*100+c)' "$VERSION")

case "$PLATFORM" in
  android) ;;
  ios) ;;
  *)
    echo "usage: $0 android|ios [version]" >&2
    exit 2
    ;;
esac

echo "patch-native: $PLATFORM version=$VERSION code=$VERSION_CODE"

if [ "$PLATFORM" = "android" ]; then
  MANIFEST="$NATIVE_DIR/android/app/src/main/AndroidManifest.xml"
  GRADLE="$NATIVE_DIR/android/app/build.gradle"
  [ -f "$MANIFEST" ] || { echo "error: $MANIFEST missing — run 'npx cap add android' first" >&2; exit 1; }

  python3 - "$MANIFEST" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p, encoding="utf-8").read()
orig = s

# 1. Cleartext http for LAN / tailnet servers via a network security config
#    (res/xml/network_security_config.xml, written below by the shell part).
if "android:networkSecurityConfig" not in s:
    s = s.replace("<application", '<application\n        android:networkSecurityConfig="@xml/network_security_config"', 1)

# 2. Camera permission for the QR pairing scanner (+ ML Kit module hint so the
#    Google code-scanner module is fetched at install time, not first use).
if 'android.permission.CAMERA' not in s:
    s = s.replace(
        "</manifest>",
        '    <uses-permission android:name="android.permission.CAMERA" />\n'
        '    <uses-feature android:name="android.hardware.camera" android:required="false" />\n'
        "</manifest>",
    )
if "com.google.mlkit.vision.DEPENDENCIES" not in s:
    s = s.replace(
        "</application>",
        '        <meta-data android:name="com.google.mlkit.vision.DEPENDENCIES" android:value="barcode_ui" />\n'
        "    </application>",
    )

# 3. cortex:// deep links → MainActivity.
if 'android:scheme="cortex"' not in s:
    filt = (
        "\n            <intent-filter android:autoVerify=\"false\">\n"
        "                <action android:name=\"android.intent.action.VIEW\" />\n"
        "                <category android:name=\"android.intent.category.DEFAULT\" />\n"
        "                <category android:name=\"android.intent.category.BROWSABLE\" />\n"
        "                <data android:scheme=\"cortex\" />\n"
        "            </intent-filter>\n"
    )
    # Insert right after the launcher intent-filter of the main activity.
    m = re.search(r"(<intent-filter>.*?android\.intent\.category\.LAUNCHER.*?</intent-filter>)", s, re.S)
    if not m:
        sys.exit("patch-native: could not find launcher intent-filter in " + p)
    s = s[: m.end()] + filt + s[m.end():]

if s != orig:
    open(p, "w", encoding="utf-8").write(s)
    print("patch-native: patched", p)
else:
    print("patch-native: manifest already patched")
PY

  NSC_DIR="$NATIVE_DIR/android/app/src/main/res/xml"
  mkdir -p "$NSC_DIR"
  # Cortex servers on a LAN or tailnet are usually plain http on a private IP
  # (Android's config can't express IP ranges, so cleartext is allowed
  # globally). TLS via `tailscale serve` still works and is preferred.
  cat > "$NSC_DIR/network_security_config.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<network-security-config>
    <base-config cleartextTrafficPermitted="true">
        <trust-anchors>
            <certificates src="system" />
        </trust-anchors>
    </base-config>
</network-security-config>
XML
  echo "patch-native: wrote $NSC_DIR/network_security_config.xml"

  if [ -f "$GRADLE" ]; then
    python3 - "$GRADLE" "$VERSION" "$VERSION_CODE" <<'PY'
import re, sys
p, ver, code = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p, encoding="utf-8").read()
s2 = re.sub(r'versionCode\s+\d+', f'versionCode {code}', s, count=1)
s2 = re.sub(r'versionName\s+"[^"]*"', f'versionName "{ver}"', s2, count=1)
if s2 != s:
    open(p, "w", encoding="utf-8").write(s2)
    print("patch-native: set versionName/versionCode in", p)
PY
  fi
fi

if [ "$PLATFORM" = "ios" ]; then
  PLIST="$NATIVE_DIR/ios/App/App/Info.plist"
  PBX="$NATIVE_DIR/ios/App/App.xcodeproj/project.pbxproj"
  [ -f "$PLIST" ] || { echo "error: $PLIST missing — run 'npx cap add ios' first" >&2; exit 1; }

  python3 - "$PLIST" <<'PY'
import plistlib, sys
p = sys.argv[1]
with open(p, "rb") as f:
    d = plistlib.load(f)
orig = dict(d)

url_types = d.setdefault("CFBundleURLTypes", [])
if not any("cortex" in (t.get("CFBundleURLSchemes") or []) for t in url_types):
    url_types.append({
        "CFBundleURLName": "com.networkbound.cortex",
        "CFBundleURLSchemes": ["cortex"],
        "CFBundleTypeRole": "Editor",
    })

d.setdefault("NSCameraUsageDescription",
             "Cortex uses the camera to scan the pairing QR code shown by the desktop app.")
d.setdefault("NSLocalNetworkUsageDescription",
             "Cortex connects to your own computer running Cortex over your LAN or Tailscale network.")

# ATS: plain-http servers on LAN / tailnet IPs. NSAllowsLocalNetworking alone
# only covers unqualified/.local hosts; a 100.x.y.z tailnet IP over http needs
# arbitrary loads. Documented risk in docs/MOBILE.md.
ats = d.setdefault("NSAppTransportSecurity", {})
ats["NSAllowsLocalNetworking"] = True
ats["NSAllowsArbitraryLoads"] = True

# Only standard TLS (exempt); skips the export-compliance question on upload.
d["ITSAppUsesNonExemptEncryption"] = False

# Mirror CFBundleShortVersionString to the Cortex version if it is the template
# placeholder; the pbxproj patch below is what Xcode actually uses.
with open(p, "wb") as f:
    plistlib.dump(d, f, sort_keys=False)
print("patch-native: patched", p)
PY

  # Privacy manifest. The Capacitor template ships ios/App/App/PrivacyInfo.xcprivacy
  # already wired into the Xcode target; we add the UserDefaults reason
  # (@capacitor/preferences) and declare no tracking. If the template ever drops
  # it, the file is still written but would need adding to the target by hand.
  PRIV="$NATIVE_DIR/ios/App/App/PrivacyInfo.xcprivacy"
  python3 - "$PRIV" <<'PY'
import os, plistlib, sys
p = sys.argv[1]
d = {}
if os.path.exists(p):
    with open(p, "rb") as f:
        d = plistlib.load(f)
d["NSPrivacyTracking"] = False
d.setdefault("NSPrivacyTrackingDomains", [])
d.setdefault("NSPrivacyCollectedDataTypes", [])
apis = d.setdefault("NSPrivacyAccessedAPITypes", [])
if not any(a.get("NSPrivacyAccessedAPIType") == "NSPrivacyAccessedAPICategoryUserDefaults" for a in apis):
    apis.append({
        "NSPrivacyAccessedAPIType": "NSPrivacyAccessedAPICategoryUserDefaults",
        "NSPrivacyAccessedAPITypeReasons": ["CA92.1"],
    })
with open(p, "wb") as f:
    plistlib.dump(d, f, sort_keys=False)
print("patch-native: patched", p)
PY

  if [ -f "$PBX" ]; then
    python3 - "$PBX" "$VERSION" "$VERSION_CODE" <<'PY'
import re, sys
p, ver, code = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p, encoding="utf-8").read()
s2 = re.sub(r'MARKETING_VERSION = [^;]+;', f'MARKETING_VERSION = {ver};', s)
s2 = re.sub(r'CURRENT_PROJECT_VERSION = [^;]+;', f'CURRENT_PROJECT_VERSION = {code};', s2)
if s2 != s:
    open(p, "w", encoding="utf-8").write(s2)
    print("patch-native: set MARKETING_VERSION/CURRENT_PROJECT_VERSION in", p)
else:
    print("patch-native: no version fields found in pbxproj (skipped)")
PY
  fi
fi

echo "patch-native: done ($PLATFORM)"
