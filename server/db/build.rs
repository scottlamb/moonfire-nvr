// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Ok(protobuf_codegen::Codegen::new()
        .pure()
        .out_dir(std::env::var("OUT_DIR")?)
        .inputs(&["proto/schema.proto"])
        .include("proto")
        .customize(protobuf_codegen::Customize::default().gen_mod_rs(true))
        .run()?)
}
