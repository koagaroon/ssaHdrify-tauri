# Bundled SQLite override

The application uses a temporary Cargo path override of `libsqlite3-sys 0.38.2`
to bundle the stable SQLite 3.54.1 release. The published sys crate bundles
SQLite 3.53.2. This is a locally maintained source patch, not a new upstream
crate release. The normal `rusqlite` bundled build still compiles SQLite into
both portable applications; no separately installed SQLite library is needed.

## Provenance and scope

[`sqlite-provenance.json`](sqlite-provenance.json) records the source archive
URLs, published archive checksums, and original/current SHA-256 for every file
in the complete published sys crate. The original archive is verified against
the [crates.io index](https://index.crates.io/li/bs/libsqlite3-sys). The SQLite
archive and `sqlite3.c` are verified against the [official download
checksums](https://sqlite.org/download.html) and [3.54.1 release
checksums](https://sqlite.org/releaselog/3_54_1.html).

To reproduce the patch:

1. Verify and unpack the recorded `libsqlite3-sys 0.38.2` archive into
   `libsqlite3-sys/`, preserving all 26 files.
2. Verify the recorded SQLite amalgamation archive. Copy its unmodified
   `sqlite3.c`, `sqlite3.h`, and `sqlite3ext.h` into the crate's `sqlite3/`
   directory.
3. In both `sqlite3/bindgen_bundled_version.rs` and
   `sqlite3/bindgen_bundled_version_ext.rs`, update only `SQLITE_VERSION`,
   `SQLITE_VERSION_NUMBER`, `SQLITE_SOURCE_ID`, `SQLITE_SCM_BRANCH`,
   `SQLITE_SCM_TAGS`, and `SQLITE_SCM_DATETIME` to match the replacement header.
   Retain the released Rust API subset and build code as reviewed below.
4. Run the vendor integrity test and the native font-lookup and cache tests.
   They check binding metadata, the linked SQLite version/source ID, query
   results and preparation behavior, and journal recovery.

Only five files differ from the released crate. The retained SQLCipher sources
and optional feature implementations are unmodified; the application does not
enable SQLCipher. The integrity test detects accidental changes to this payload,
including the license text. Git preserves its bytes without line-ending conversion.

SQLite's [journal recovery hardening](https://sqlite.org/src/info/9471fa2c9c)
requires an expected super-journal filename and a reference back to the journal
being recovered before removing a referenced file. Both the former SQLite
3.51.3 bundle and the published 3.53.2 bundle lack this fix. Retaining the older
crate was not evidence that this recovery path was safer.

## Binding compatibility

The reviewed 3.53.4-to-3.54.1 main-header change adds `sqlite3_incomplete()`,
`sqlite3_result_str()`, and four constants; existing function declarations,
types, and numeric constants remain compatible. The extension header appends
the two new function pointers and makes the existing C calling convention
explicit for variadic functions. It does not reorder the existing fields.

The application uses the ordinary static `bundled` feature, not
`loadable_extension` or the new interfaces. Its retained Rust declarations
remain a compatible subset; the ordinary bindings treat the extension API
table as opaque, and the optional extension bindings retain its existing
prefix with `extern "C"` functions. This patch does not expose the newly added
interfaces through Rust or claim newly generated bindings. Review any new
interface or optional-feature use before relying on it.

This five-file patch and metadata update are specific to the reviewed source.
Compare C declarations, constants, public types, and the extension-table layout
again for a future release; version metadata alone cannot establish compatibility.

## Licensing

The Rust bindings/build code retain their full [MIT license](libsqlite3-sys/LICENSE).
SQLite's deliverable source is [public domain](https://sqlite.org/copyright.html).
The optional, unused SQLCipher payload retains its own
[license](libsqlite3-sys/sqlcipher/LICENSE). The GUI and CLI offline notices
include the Rust binding license and SQLite attribution.

## Lookup regression coverage

SQLite 3.54.1 includes the [fix for repeated query preparation](https://sqlite.org/src/info/7f154192db)
and the [unchanged-integer binding fix](https://sqlite.org/src/info/4708cfe5f7).
An initial binding of a 0/1 parameter can still cause one reprepare so SQLite
can optimize for that value. A zero-reprepare requirement for a fresh statement
would reject this intended behavior along with the actual repeated-preparation
regression in 3.54.0.

The native session-query test checks correct results, permits at most one
initial reprepare, and rejects further reprepares when reusing the same
statement with unchanged bindings. Production queries and SQLite settings are
unchanged. Controlled Windows x64 comparisons found about 14–17 microseconds
of additional SQL work per fresh lookup against 3.53.4, with correct results;
this is a small added cost, not a performance improvement. Dependency Watch
has no source hold for this release. New stable sources still require review.

## Removing the override

Dependency Watch checks both `rusqlite` and the resolved `libsqlite3-sys`
override, requiring manual review of new releases of either package. Remove
the Cargo patch and this vendor directory together when a reviewed published
`libsqlite3-sys` release provides the required recovery hardening and compatible
query behavior, preserves the intended static bundled build, and passes the
font-lookup and cache recovery tests and the project's Rust
minimum-version check. Update the notice source, explicit override monitor,
and integrity test in the same change; keep the runtime hardening floor and
behavioral recovery tests.
