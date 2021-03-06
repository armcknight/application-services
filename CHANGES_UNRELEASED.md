**See [the release process docs](docs/howtos/cut-a-new-release.md) for the steps to take when cutting a new release.**

# Unreleased Changes

[Full Changelog](https://github.com/mozilla/application-services/compare/v0.55.0...master)

## General

### What's changed

- iOS: Xcode version changed to 11.4.0 from 11.3.1.

## Logins

### ⚠️ Breaking changes ⚠️

- Android: `MemoryLoginsStorage` has been removed. Use DatabaseLoginsStorage(":memory:") instead.
  ([#2833](https://github.com/mozilla/application-services/pull/2823)).

## Libs

### What's changed

- The project now builds with version 4.3.0 of SQLCipher instead of a fork
  of version 4.2.0. Newest version has NSS crypto backend. ([#2822](https://github.com/mozilla/application-services/pull/2822)).

## FxA Client

### Breaking changes

- `Server.dev` is now `Server.stage` to reflect better the FxA server instance it points to. ([#2830](https://github.com/mozilla/application-services/pull/2830)).
