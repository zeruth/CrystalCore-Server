# Captures, build 672261

Raw frames as the client sent them, back to back (`[lobby] capture_dir` writes these). Tests replay
them, so every file here must stay byte-exact.

| file | what |
|---|---|
| `lobby-encryption-init.bin` | The β client (`ffxiv.exe`, launcher arguments, `DEV.TestSID=0`) against a lobby that never answers: four identical 664-byte frames, one every 2 s, each one `EncryptionInit` segment with key phrase "Test Ticket Data" and key `0x6ade798e`. Captured 2026-10-25. |
