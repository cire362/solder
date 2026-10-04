# What runs JavaScript plugins

`qjs-wasi.wasm` is QuickJS-ng 0.17.0 built for WASI, exactly as released:

    https://github.com/quickjs-ng/quickjs/releases/download/v0.17.0/qjs-wasi.wasm
    sha256 42a732a676ec2d93488c19411e0fad283bf72658fdad746f089914b523c783b1

It is compiled into the editor and runs inside the same sandbox as every
other plugin. Its license (MIT) is in `QUICKJS-LICENSE`. To move to another
release, replace the file, update the hash here and in `src/wasi.rs`, and run
the tests.
