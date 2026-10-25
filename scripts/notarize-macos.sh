#!/usr/bin/env bash
# Sign with a Developer ID, submit to Apple for notarization and staple the ticket to the DMG.
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" NOTARY_PROFILE=codeyard-notary scripts/notarize-macos.sh
# Needs a paid Apple Developer account, a Developer ID Application certificate in the keychain and,
# once:
#   xcrun notarytool store-credentials codeyard-notary --apple-id <email> --team-id <TEAMID>
# Status: never run, there is no Developer account yet; the steps follow Apple's documentation.
# After the first success, drop the "not notarized" warning from the README and docs/platforms.md.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${SIGN_IDENTITY:?set SIGN_IDENTITY (Developer ID Application: ...)}"
: "${NOTARY_PROFILE:?set NOTARY_PROFILE (the notarytool store-credentials name)}"
scripts/bundle-macos.sh
DMG=target/release/Codeyard.dmg
xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
echo "notarized and stapled: $DMG"
