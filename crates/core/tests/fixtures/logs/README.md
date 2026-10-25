Log excerpts in the formats the frameworks write, for the load-status tests.

The owner's game has no RED4ext, redscript or CET logs yet, so these are
written from the frameworks' source rather than copied from a real run:

- `red4ext-2026-10-25-14-03-11.log`: RED4ext `src/dll/App.cpp` (file name),
  `src/dll/Utils.cpp` (line pattern `[%Y-%m-%d %H:%M:%S.%e] [%^%-8l%$] [%6t] [%n] %v`)
  and `src/dll/Systems/PluginSystem.cpp` (messages; plugins load one at a
  time, and the "has been loaded" line carries the plugin's own name, so
  `mod_settings` logs as "Mod Settings").
- `scripting.log`: Cyber Engine Tweaks `src/Utils.h` (pattern
  `[%Y-%m-%d %H:%M:%S UTC%z] [%t] %v`), `src/scripting/Scripting.cpp` and
  `src/scripting/ScriptStore.cpp` (messages).
- `redscript_rCURRENT.log`: redscript 0.5.32 `scc/lib/src/lib.rs` (format
  `[{level} - {rfc2822}] {message}`, messages) and `compiler/src/source_map.rs`
  (one compiled file per line).

Replace them with trimmed real logs once a modded game has run.
