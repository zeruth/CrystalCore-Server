//! Packet layouts and opcodes of ffxiv.exe 672261 (the β client), both directions, so the server
//! and the headless clients (`crystal-bot`) are built from the same definitions. The lobby's
//! packets still live with its encryption in `crystal-lobby`.

pub mod zone;
