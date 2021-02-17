// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Ok(protobuf_codegen_pure::Codegen::new()
        .out_dir(std::env::var("OUT_DIR")?)
        .inputs(&["proto/schema.proto"])
        .include("proto")
        .customize(protobuf_codegen_pure::Customize {
            gen_mod_rs: Some(true),
            ..Default::default()
        })
        .run()?)
}
