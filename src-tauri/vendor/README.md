# Bundled SQLite override

The application uses a temporary Cargo path override of `libsqlite3-sys 0.38.2`
to bundle the stable SQLite 3.53.4 release. The published sys crate bundles
SQLite 3.53.2. This is a locally maintained source patch, not a new upstream
crate release. The normal `rusqlite` bundled build still compiles SQLite into
both portable applications; no separately installed SQLite library is needed.

## Provenance and scope

[`sqlite-provenance.json`](sqlite-provenance.json) records the source archive
URLs, published archive checksums, and original/current SHA-256 for every file
in the complete published sys crate. The original archive is verified against
the [crates.io index](https://index.crates.io/li/bs/libsqlite3-sys). The SQLite
archive and `sqlite3.c` are verified against the [official download
checksums](https://sqlite.org/download.html) and [3.53.4 release
checksums](https://sqlite.org/releaselog/3_53_4.html).

To reproduce the patch:

1. Verify and unpack the recorded `libsqlite3-sys 0.38.2` archive into
   `libsqlite3-sys/`, preserving all 26 files.
2. Verify the recorded SQLite amalgamation archive. Copy its unmodified
   `sqlite3.c`, `sqlite3.h`, and `sqlite3ext.h` into the crate's `sqlite3/`
   directory. `sqlite3ext.h` is unchanged from the original crate.
3. In both `sqlite3/bindgen_bundled_version.rs` and
   `sqlite3/bindgen_bundled_version_ext.rs`, update only `SQLITE_VERSION`,
   `SQLITE_VERSION_NUMBER`, `SQLITE_SOURCE_ID`, `SQLITE_SCM_TAGS`, and
   `SQLITE_SCM_DATETIME` to match the replacement header. Its remaining API
   declarations are unchanged; retain the released Rust bindings and build code.
4. Run the vendor integrity test and the native cache tests. The latter check
   the linked SQLite version/source ID and journal recovery behavior.

Only four files differ from the released crate. The retained SQLCipher sources
and optional feature implementations are unmodified; the application does not
enable SQLCipher. The integrity test detects accidental changes to this payload,
including the license text. Git preserves its bytes without line-ending conversion.

SQLite's [journal recovery hardening](https://sqlite.org/src/info/9471fa2c9c)
requires an expected super-journal filename and a reference back to the journal
being recovered before removing a referenced file. Both the former SQLite
3.51.3 bundle and the published 3.53.2 bundle lack this fix. Retaining the older
crate was not evidence that this recovery path was safer.

## Licensing

The Rust bindings/build code retain their full [MIT license](libsqlite3-sys/LICENSE).
SQLite's deliverable source is [public domain](https://sqlite.org/copyright.html).
The optional, unused SQLCipher payload retains its own
[license](libsqlite3-sys/sqlcipher/LICENSE). The GUI and CLI offline notices
include the Rust binding license and SQLite attribution.

## Removing the override

Dependency Watch checks both `rusqlite` and the resolved `libsqlite3-sys`
override, requiring manual review of new releases of either package. Remove
the Cargo patch and this vendor directory together when a reviewed published
`libsqlite3-sys` release bundles SQLite 3.53.4 or newer, preserves the intended
static bundled build, and passes cache recovery tests and the project's Rust
minimum-version check. Update the notice source, explicit override monitor,
and integrity test in the same change; keep the runtime hardening floor and
behavioral recovery tests.
