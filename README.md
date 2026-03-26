# ytplay

`ytplay` is a small Rust CLI that plays audio from a YouTube URL with low overhead by delegating extraction to `yt-dlp` and playback to `mpv`.

## How to use

Install the runtime dependencies with Homebrew:

```bash
brew install yt-dlp mpv
```

Run from the repo with a URL:

```bash
cargo run -- 'https://www.youtube.com/watch?v=dQw4w9WgXcQ'
```

Or use a bare YouTube video ID without quotes:

```bash
cargo run -- dQw4w9WgXcQ
```

After playback starts, `ytplay` clears the terminal and shows:

- the video title
- a progress bar with elapsed and total time
- keyboard controls for playback and audio
- a 0-100 volume scale where the default mpv level shows as 50%
- an `Up Next` queue based on search results from the current video title/uploader

Or run without an argument and paste a URL when prompted:

```bash
cargo run
```

Or pipe the URL on stdin:

```bash
printf '%s\n' 'https://www.youtube.com/watch?v=dQw4w9WgXcQ' | cargo run
```

If you want a local release binary:

```bash
cargo build --release
./target/release/ytplay 'https://www.youtube.com/watch?v=dQw4w9WgXcQ'
```

The binary interface is:

```bash
ytplay <url>
```

If you are using `zsh`, quote full YouTube URLs on the command line. Characters like `?` and `&` are interpreted by the shell before `ytplay` sees them.
This is a shell parsing issue, not a terminal-emulator issue, so unquoted full watch URLs cannot be made reliable from inside the Rust program itself. If you want an unquoted one-liner, use a bare video ID instead.

## Controls

While playback is active in an interactive terminal:

- `P` toggles play and pause
- `J` skips back 30 seconds
- `L` skips forward 30 seconds
- `U` lowers volume
- `I` raises volume
- `M` toggles mute
- `N` opens or closes the `Up Next` panel
- `Q` quits playback

The UI volume is capped between `0%` and `100%`. `0%` maps to silence, and the default mpv level appears as `50%`.

## Up Next

While a track is playing, press `N` to open the `Up Next` panel.

- number keys queue one of the listed recommendations shown on screen
- arrow keys move the highlighted selection
- `Enter` keeps the currently selected recommendation
- `Esc` or `N` closes the panel while the current track keeps playing

When the current track finishes:

- the selected recommendation auto-plays immediately
- if you never picked one, `ytplay` defaults to recommendation `1` after a short countdown

## Why this design

- Keeps the Rust binary small and simple
- Avoids embedding a downloader, decoder, or browser engine
- Streams audio only, which keeps memory use and startup time low

## Behavior

- Uses `yt-dlp --no-playlist --no-warnings -f bestaudio/best --get-url`
- Launches `mpv` in audio-only mode
- Uses mpv's JSON IPC to render a lightweight in-terminal playback UI
- Uses conservative cache settings to keep memory usage low
- Returns mpv's exit code when playback ends

## Scope

`ytplay` v1 handles one URL at a time. It does not support playlists, downloads, search, or queue management.
