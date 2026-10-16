# Secret references for a signed + notarized macOS release.
#
# This file is safe to commit: it contains only op:// pointers, never values.
# `op run --env-file` resolves them at launch and injects them into the build
# process, so the certificate never lands on disk and never enters a shell
# history. See scripts/build-macos-release.sh.
#
# All of these live on one 1Password item ("Apple App Specific Password",
# Private vault). That item holds two Developer ID certificates; the section
# id below selects the one expiring 2031-04-14. The other section
# (cq4d2obdnqvlw6obucelfsf77y) holds an older cert for the same identity.

# Base64-encoded Developer ID Application .p12 and its password.
APPLE_CERTIFICATE="op://Private/Apple App Specific Password/m3ulgzm7rgh362gcialg733w2a/Cert - Base64"
APPLE_CERTIFICATE_PASSWORD="op://Private/Apple App Specific Password/m3ulgzm7rgh362gcialg733w2a/Cert Password"

# Not a secret — it is printed in the certificate subject — but kept here so
# the identity the build signs with is declared in exactly one place.
APPLE_SIGNING_IDENTITY="Developer ID Application: MICHAEL ALLEN STEPHENS (YMPSC6RQ4F)"

# Notarization: Apple ID plus an app-specific password (not the account password).
APPLE_ID="op://Private/Apple App Specific Password/username"
APPLE_PASSWORD="op://Private/Apple App Specific Password/password"
APPLE_TEAM_ID="op://Private/Apple App Specific Password/add more/Team ID"
