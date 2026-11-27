# The launcher

A window to start the game with the fork's settings, and a set of command-line modes that do
the same without a window. It is PowerShell and WPF, already on every Windows: nothing to
install. It runs on Windows PowerShell 5.1 and on PowerShell 7.

- **Double-click `launcher.cmd`** in the repository's folder: the window opens and no console
  window stays open.
- The first time, it asks for the executable of the game you dumped (a file picker). It is
  saved and asked again only if the file is not there any more.
- **Play** checks for `target\<build>\victory.exe` (it never builds it: when it is missing it
  says which `cargo build` command makes it), prepares what has to be prepared, and starts the
  game through `scripts\play.ps1`. The window hides while the game runs and comes back when it
  exits.

Nothing of the game is in the launcher or in this repository (docs/asset-policy.md): no name,
logo or picture. The only page it ever opens is the fork's GitHub page, from the link at the
bottom of the options panel; the version beside it is `v<workspace version> (<short commit>)`.

## What Play does

| Step | When | What runs |
|---|---|---|
| Compile | always | only a check that `target\<build>\victory.exe` is there |
| Translate the shaders | `victory.exe` changed, or a switch the shader cache is keyed on changed (`VICTORY_RT_MISS`, which ray tracing turns off, and the others listed in `options.json`, taken from `crates/victory-gpu/src/shader_cache.rs`) | `victory.exe shaders <the game's folder> --jobs <threads>` |
| Warm up the pipelines | after a run drew with pipelines not made ahead yet, or after the shaders were translated again (never before the first run: `warm --used` makes the pipelines earlier runs wrote down) | `victory.exe warm --used --jobs <threads>` |
| Open the game | always | `scripts\play.ps1 -Exe <game> -Build <build> -Console <profile> [-RayTracing] -More @{...}` |

What was done is kept in `prepared.json`, next to the saved settings, one entry per build
(the SHA-256 of `victory.exe`, the hash of the translation switches, and when the shaders,
the warm-up and the last run happened). Each step's output goes to `logs\<step>.log` in the
same folder.

## The options

`scripts/launcher/options.json` describes every option: its id (stable: the MCP server takes
it), kind, group, default and, for each value, the `play.ps1` parameters and the `VICTORY_*`
switches it sets. An option marked "not on this version" has its code on another branch: it
is shown disabled and never sent.

| Id | Kind | Default | Values and what they set | On this version |
|---|---|---|---|---|
| `profile` | choice | `series-s` | `series-s`, `series-x`: `play.ps1 -Console` | yes |
| `image` | choice | `native` | `native`: the title's own upscaler (`VICTORY_UPSCALER=6`, from `play.ps1`); `dlss`: `VICTORY_DLSS=1`, `VICTORY_UPSCALER` unset | `dlss` no |
| `frame_generation` | toggle | off | `VICTORY_FRAMEGEN=1`; needs `image=dlss` (turning it on sets it; `image=native` turns it off) | no |
| `reflex` | toggle | off | `VICTORY_REFLEX=on` | no |
| `ray_tracing` | toggle | off | on: `play.ps1 -RayTracing` (`VICTORY_BVH_BUILDER=1`); off: `VICTORY_RT_MISS=1`, as `play.ps1` does | yes |
| `light_day_night` | toggle | on | `VICTORY_AMBIENT_DAY=0.5:2`, `VICTORY_AMBIENT_CONTRAST=4:8`, `VICTORY_AMBIENT_CUT=3`, `VICTORY_AMBIENT_AIR_SOFT=1`, `VICTORY_AMBIENT_EDGE=4` | no (the ambient switches are not merged yet) |
| `water_light` | toggle | on | off: `VICTORY_WATER_LIGHT=0` (read on cards of 32-lane waves) | yes |
| `outdoor_brightness` | choice | `default` | `VICTORY_AMBIENT_OUTSIDE`: `default` 1:0.16, `darker` 1:0.08, `brighter` 1:0.25 | yes |
| `thin_lists` | toggle | on | `VICTORY_THIN=1`, off `VICTORY_THIN=0` | yes |
| `card_memory` | choice | `1024` | `512`, `1024`, `2048`: `VICTORY_MEMORY_CARD_MB` | yes |
| `jobs` | choice | `20` | `8`, `12`, `20`: `--jobs` of the shader and warm-up steps | yes |
| `build` | choice | `release` | `release`, `diag`: `play.ps1 -Build` (`target\release` or `target\diag`) | yes |
| `capture_frames` | toggle | off | `VICTORY_CAPTURE=<settings folder>\captures`; needs `build=diag` (turning it on sets it; `build=release` turns it off) | yes |
| `detailed_log` | toggle | off | `VICTORY_LOG=debug` (debug lines exist only in the diag build; the default filter is `info`) | yes |

With every option at its default the run gets exactly the switches `scripts\play.ps1` sets
with no parameter: `launcher.ps1 -Check` proves it by reading what `play.ps1` sets from the
script itself and comparing. The launcher's build default is `release` (the build the
README's benchmark is of; `play.ps1`'s own default is `diag`).

There is no "start from" choice: nothing on this branch picks between the last save and a
new game.

## The saved settings

`%APPDATA%\VI-BR-PORT\launcher.json` (`VICTORY_LAUNCHER_CONFIG=<file>` puts it elsewhere; the
self-check uses its own). Every change, in the window or through the MCP server, is written at
once, through a temporary file moved into place, as UTF-8 without BOM. The window and the MCP
server share the file: the window reads it again when it changes on disk.

```json
{
  "format": 1,
  "options": { "ray_tracing": true, "thin_lists": true },
  "game_exe": "D:\\dump\\<the game's executable>",
  "language": "pt-BR"
}
```

An option not in the file has its default; a value this version does not know is left in
the file and the default is used. Keys the launcher does not know are kept. A file that does
not read is set aside as `launcher.json.bad` and the defaults are used. `language` is `en` or
`pt-BR` (`null`: Portuguese on a Portuguese Windows, English on any other).

## Command-line modes

```powershell
scripts\launcher.ps1 -Check                  # the self-check (no window, nothing started)
scripts\launcher.ps1 -Get                    # the saved settings, as JSON
scripts\launcher.ps1 -Set ray_tracing=on     # one option (on/off, true/false, or a listed value)
scripts\launcher.ps1 -Set game_exe=D:\dump\<executable>
scripts\launcher.ps1 -Set language=en
scripts\launcher.ps1 -Reset                  # every option back to its default
scripts\launcher.ps1 -PrintEnv               # play.ps1's parameters, the switches, the steps and their command lines
scripts\launcher.ps1 -Version                # v0.1.0 (<commit>)
scripts\launcher.ps1 -Snapshot play.png -View more -Language en   # the window drawn off screen, never shown
```

`launcher.cmd` takes the same arguments (`launcher.cmd -Get`).

## The window

960 x 640, not resizable, in the colours and sizes of `scripts/launcher/tokens.json`. The
**Play** view has the Play button (or **Choose the executable** while none is saved) and the
options panel: profile, image, generated frames and ray tracing, **More options**, the GitHub
link and the version. **More options** has every other option, by group, with **Restore the
defaults**; a change is saved the moment it is made. Options that are not on this branch are
drawn disabled and off, with "Not on this version yet". **Preparing** shows the four steps and a
progress bar while Play prepares; a step that fails leaves its message, its log and the command
to run by hand. Every control is reached with Tab and has a gold focus rectangle and a name for
screen readers. When the window opens the sunset fades in from the night colour and the sun's
glow breathes slowly; with the animations of Windows turned off, nothing moves.

The window reads the saved file again when it changes on disk, so an option changed through the
MCP server or `-Set` shows in an open window within a second.

## The MCP server

`scripts\launcher-mcp.ps1` lets an MCP client (Claude Code, Claude Desktop or any other)
read and change the launcher's options. It works on the same saved file as the window, so an
option turned off through it shows as off in an open window. **It only changes settings.** No
tool starts the game, builds, or runs a command. No tool takes a path, a command or a free
environment variable. The only file it writes is the saved settings file.

It speaks the Model Context Protocol over stdio: newline-delimited JSON-RPC 2.0, UTF-8 without
BOM. Only protocol messages go to stdout; its one log line goes to stderr. It handles
`initialize` (it answers with the client's protocol version when it supports it, else with
the newest it supports: 2025-11-25, 2025-06-18, 2025-03-26 or 2024-11-05),
`notifications/initialized`, `ping`, `tools/list` and `tools/call`. It also answers a batch
(a JSON array). A method it does not know gets -32601. Bad params, an unknown tool or
arguments outside a tool's schema get -32602. A line that is not JSON gets -32700, and the
server keeps going.

| Tool | Arguments | What it does |
|---|---|---|
| `launcher_list_options` | `language?` (`en`, `pt-BR`) | every option: id, label, hint, group, kind, values (with their labels and whether they are on this branch), default, current value, `available`, and why not when it is not |
| `launcher_get_config` | none | the saved settings: every option's value, `game_exe`, `language`, the file's path |
| `launcher_set_option` | `id`, `value` | sets one option and saves at once. `value` is true/false (or on/off) for an on/off option and one of the listed values for a choice. Rules between options apply. The answer names every option that moved, the ones a rule moved too |
| `launcher_enable` | `id` | turns an on/off option on and saves |
| `launcher_disable` | `id` | turns an on/off option off and saves |
| `launcher_reset` | `id?` | puts that option, or every option, back to its default and saves. `game_exe` and `language` stay |
| `launcher_env` | none | a dry run of Play: the `play.ps1` arguments, the `-More` switches, every `VICTORY_*` switch of the run, `--jobs`, and the steps with their command lines. Nothing is started |
| `launcher_version` | none | the version and commit, read once when the server starts |

A tool refuses, with `isError: true` and the reason as plain text, when:

- the id is not in `options.json`;
- the value is not one the option takes;
- the option or the value is not on this branch (`available: false`). The note from
  `options.json` says why.

An example (stdin lines start with `>`, stdout lines with `<`; the result's text is shortened):

```text
> {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"example","version":"1"}}}
< {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"vi-br-port-launcher","version":"0.1.0"},"instructions":"Reads and changes the launcher's saved options, the same ones its window shows."}}
> {"jsonrpc":"2.0","method":"notifications/initialized"}
> {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"launcher_enable","arguments":{"id":"capture_frames"}}}
< {"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\n  \"id\": \"capture_frames\",\n  \"value\": true,\n  \"changed\": {\n    \"build\": \"diag\",\n    \"capture_frames\": true\n  },\n  \"saved\": \"C:\\\\Users\\\\<you>\\\\AppData\\\\Roaming\\\\VI-BR-PORT\\\\launcher.json\",\n  \"options\": {...}\n}"}],"isError":false}}
> {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"launcher_enable","arguments":{"id":"reflex"}}}
< {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"'reflex' is not on this version yet: NVIDIA Reflex is on another branch."}],"isError":true}}
> {"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"launcher_set_option","arguments":{"id":"jobs","value":"64"}}}
< {"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"'64' is not a value of 'jobs'. The values are: 8, 12, 20."}],"isError":true}}
```

Turning `capture_frames` on also moved `build` to `diag`, because frame capture needs the
diagnostics build.

### Registering it

In a client that takes a JSON configuration (Claude Desktop's `claude_desktop_config.json`,
a project's `.mcp.json`):

```json
{
  "mcpServers": {
    "vi-br-port-launcher": {
      "command": "pwsh",
      "args": ["-NoProfile", "-File", "<repository>\\scripts\\launcher-mcp.ps1"]
    }
  }
}
```

With Windows PowerShell instead of PowerShell 7, use `"command": "powershell"` and
`"args": ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "<repository>\\scripts\\launcher-mcp.ps1"]`.
With Claude Code:

```powershell
claude mcp add vi-br-port-launcher -- pwsh -NoProfile -File <repository>\scripts\launcher-mcp.ps1
```

`VICTORY_LAUNCHER_CONFIG=<file>` in the server's environment points it at another settings
file.

### Its self-check

`scripts\launcher\Test-Mcp.ps1` starts the server as a child process with no window, on a
temporary settings file. It talks to the server as a client does and checks every answer and
the saved file: initialize, tools/list, a set, an enable, a disable, the rules between
options, a refused id, a refused value, an option not on this branch, arguments outside a
schema, a malformed line, a batch, the dry run and both resets. It also checks that stdout
has no BOM and nothing but protocol lines, and that the server wrote no file but the settings.
The server runs on the same PowerShell as the check, or on the one `-Shell` names:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\launcher\Test-Mcp.ps1
pwsh -NoProfile -File scripts\launcher\Test-Mcp.ps1
pwsh -NoProfile -File scripts\launcher\Test-Mcp.ps1 -Shell powershell.exe   # a 5.1 server, checked from 7
```

## Checking the whole launcher

Nothing here opens a window or starts the game:

```powershell
scripts\launcher.ps1 -Check                                     # the self-check
scripts\launcher\Test-Mcp.ps1                                   # the MCP server's self-check
scripts\launcher.ps1 -Snapshot play.png -View play -Language en # each view (play, preparing, more), in each language
```

Run the first two under both `powershell` and `pwsh`.

## Files

| File | What it holds |
|---|---|
| `launcher.cmd` | the double-click entry |
| `scripts/launcher.ps1` | the entry script and its command-line modes |
| `scripts/launcher-mcp.ps1` | the MCP server |
| `scripts/launcher/options.json` | every option |
| `scripts/launcher/tokens.json` | the design system: colours, type, sizes, radii, motion |
| `scripts/launcher/lang/en.json`, `pt-BR.json` | every visible text |
| `scripts/launcher/Launcher.ps1` | loads the parts below |
| `scripts/launcher/Json.ps1` | JSON read and written the same on both PowerShells |
| `scripts/launcher/Config.ps1` | the saved settings |
| `scripts/launcher/Options.ps1` | options.json, setting an option, resolving a run |
| `scripts/launcher/PlayScript.ps1` | what `scripts/play.ps1` sets, read from its syntax tree |
| `scripts/launcher/Game.ps1` | the steps of Play and its dry run |
| `scripts/launcher/Text.ps1`, `Version.ps1` | texts and the version |
| `scripts/launcher/Theme.ps1` | the window's brushes, fonts, icons and control templates, made from `tokens.json` |
| `scripts/launcher/Views.ps1` | the XAML of the scene and of the Play, Preparing and More options views |
| `scripts/launcher/Window.ps1` | the window: saving each change, reading the saved file again, the fades |
| `scripts/launcher/PlayRunner.ps1` | Play on a background runspace, its progress, the hidden window while the game runs |
| `scripts/launcher/Snapshot.ps1` | `-Snapshot`: the window drawn into a PNG |
| `scripts/launcher/Test-Launcher.ps1` | the self-check |
| `scripts/launcher/McpTools.ps1`, `McpProtocol.ps1` | the MCP server's tools and its JSON-RPC part |
| `scripts/launcher/Test-Mcp.ps1` | the MCP server's self-check |

## Em português

Dê dois cliques em `launcher.cmd`: a janela abre sem deixar console aberto. Na primeira vez
ela pede o executável do jogo que você extraiu e guarda. **Jogar** confere se há
`target\<build>\victory.exe` (se não houver, mostra o comando `cargo build` que o faz),
traduz os shaders e aquece os pipelines só quando precisa, e abre o jogo pelo
`scripts\play.ps1`. As opções ficam salvas em `%APPDATA%\VI-BR-PORT\launcher.json` assim que
mudam, pela janela ou pelo servidor MCP; o padrão de todas dá exatamente o que o `play.ps1`
usa sem parâmetros (`scripts\launcher.ps1 -Check` confere). Opções cujo código está em outro
branch (DLSS, quadros gerados, Reflex, luz de dia e de noite) aparecem desativadas e nunca são
enviadas. No painel de opções ficam o link para a página do fork no GitHub e a versão
(`v<versão> (<commit>)`); ao abrir, o pôr do sol aparece num fade a partir da cor da noite.
Pela linha de comando: `-Get`, `-Set ray_tracing=on`, `-Reset`, `-PrintEnv`,
`-Version`.

O servidor MCP (`scripts\launcher-mcp.ps1`) deixa um cliente MCP listar as opções, ligar e
desligar (`launcher_enable`, `launcher_disable`), mudar um valor (`launcher_set_option`),
voltar ao padrão (`launcher_reset`) e ver com o que o Jogar rodaria (`launcher_env`, sem rodar
nada). Ele só muda configurações, no mesmo arquivo que a janela usa. Não abre o jogo, não
compila e não roda comando. Para registrar, use o JSON da seção "Registering it" acima. Para
conferir, rode `scripts\launcher\Test-Mcp.ps1`.
