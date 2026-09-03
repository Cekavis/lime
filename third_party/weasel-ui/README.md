# Lime vendored WeaselUI

This directory contains the `WeaselUI` rendering sources from `rime/weasel` at
the immutable revision recorded in [`UPSTREAM_COMMIT`](UPSTREAM_COMMIT).

Lime intentionally vendors only the UI sources and WTL headers.  It does not
embed `WeaselTSF`, `WeaselServer`, Rime IPC, or the Weasel installer.  Lime's
TSF adapter remains the owner of key handling, preceding-text reads, paging,
selection, and commits.

Local changes are deliberately small:

- Boost serialization includes are optional because Lime does not use Weasel's
  IPC serialization layer.
- A `UI::SetPrecedingText` sidecar is rendered through Weasel's auxiliary row
  (using the existing preedit text color);
  the upstream `Context` wire layout is unchanged.
- Candidate mouse-up hit testing is resolved in the panel, while Lime's adapter
  forwards selection and paging intents as synthetic keyboard events so TSF
  remains the owner of context edits and commits.
- A few source-level compatibility fixes keep the snapshot buildable with the
  current MSVC/Windows SDK (temporary GDI+ values, explicit `INT` overloads,
  and public WTL double-buffer inheritance).

The upstream code is GPLv3.  Keep [`LICENSE.txt`](LICENSE.txt) with any binary
distribution and publish the corresponding modified source.  If this snapshot
is replaced by a GitHub fork or submodule, retain the revision pin and the same
source/notice obligations.
