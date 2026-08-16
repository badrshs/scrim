# Project rules for movie-plur (PureFrame Player)

## README is the source of truth - keep it in sync
`README.md` documents the complete business logic of this application, A to
Z. **Any change to `player_app.py`, `pfplay.py`, `livescan.py`,
`casting.py`, scan behavior, tunable constants, file formats, or
dependencies MUST update the matching section of `README.md` in the same
change.** If a change adds a new tunable, add it to the tunables table. If
it adds a limitation, add it to the known-limitations list. Never let the
README describe behavior the code no longer has.

## Non-negotiable design rule: fail closed
An uncensored frame must never reach the screen or a cast device. When in
doubt, over-censor (full-frame cover) rather than risk showing anything.
Missing/invalid plan = refuse to play in Scanned-plan mode. Live mode must
never allow playback or seeking past the detection frontier.

## Scope of censoring
Only explicit nudity and visible sex acts (NudeNet EXPLICIT_LABELS). No
kissing detection, no audio analysis, audio track is never modified. Do not
widen this scope without the owner asking for it.

## Practical notes
- Run everything with the venv python: `.venv\Scripts\python.exe`.
- The single-chain filtergraph design is a hard constraint: one
  crop/censor/overlay chain whose coordinates move via time expressions.
  Chains-per-window kills mpv's filter throughput (measured).
- mpv IPC pipe must stay in PIPE_NOWAIT mode; blocking reads on the duplex
  handle deadlock writes from the UI thread.
- pureframe's batch scanner stalls on long videos on this machine; the
  livescan module is the trusted scan path.
- Scans must run with HF_HUB_OFFLINE=1 once models are cached.
- Test changes with the real files here: `sample.mp4` (2 min, plan has
  zero nudity) and `abc.mp4` (61 min, plan has 66 nudity windows).
