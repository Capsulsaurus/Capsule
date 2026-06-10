//! `capsule demo` — an offline, end-to-end showcase of the cryptographic data plane.
//!
//! Runs the whole flow with **real cryptography and no network**: account + keys → album +
//! authority → import (encrypt, signed manifest, provenance, signed sidecar, `verify_asset`)
//! → CRDT metadata edits → soft-delete + restore → backup export → restore into a fresh
//! library → byte-equal verification → Shamir 2-of-3 recovery. Every step writes real
//! artifacts the user can inspect.

use std::path::PathBuf;

use capsule_core::backup::{recover_seed, split_seed_2of3};
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::lifecycle::Workspace;
use colored::*;
use eyre::{Result, eyre};

/// Fast Argon2id parameters — this is a demo; the wrap-key strength is not the point.
const DEMO_KDF: Argon2Params = Argon2Params {
    mem_kib: 8 * 1024,
    t_cost: 1,
    p_cost: 1,
};

fn step(n: u32, title: &str) {
    println!("\n{} {}", format!("[{n}]").bold().cyan(), title.bold());
}

fn ok(msg: impl AsRef<str>) {
    println!("    {} {}", "✓".green(), msg.as_ref());
}

fn info(label: &str, value: impl std::fmt::Display) {
    println!("    {} {}", format!("{label}:").dimmed(), value);
}

/// Run the showcase. `workdir` defaults to a fresh temp directory; `image` defaults to a
/// small synthetic file.
pub(crate) fn run(workdir: Option<PathBuf>, image: Option<PathBuf>) -> Result<()> {
    let root = match workdir {
        Some(p) => p,
        None => std::env::temp_dir().join(format!("capsule-demo-{}", std::process::id())),
    };
    std::fs::create_dir_all(&root)?;
    let source_lib = root.join("source-library");
    let fresh_lib = root.join("restored-library");
    let backup_path = root.join("backup.tar");

    println!(
        "{}",
        "Capsule offline data-plane showcase (real crypto, no network)"
            .bold()
            .underline()
    );
    info("workdir", root.display());

    // ── 1. Account + device keys ────────────────────────────────────────────
    step(1, "Create account + device keys");
    let mut ws = Workspace::create_with_params(&source_lib, b"demo-passphrase", DEMO_KDF)
        .map_err(|e| eyre!("create workspace: {e}"))?;
    ok(
        "master key generated; identity (IK), device signing (DSK), and device encryption (DEK) keys created",
    );
    info("user_id", ws.user_id());
    info(
        "default album id (derived from master key)",
        ws.default_album_id(),
    );

    // ── 2. Album + MLS-attested authority ───────────────────────────────────
    step(
        2,
        "Create a container album (mint AMK + write-tier + admin keys)",
    );
    let album = ws.create_album("Trip to the Coast");
    ok("AMK_v1 minted; admin-signed authority attests epoch 1");
    info("album_id", album);

    // ── 3. Import a real file ────────────────────────────────────────────────
    step(
        3,
        "Import an asset (encrypt → sign manifest → provenance → signed sidecar → verify_asset)",
    );
    let image_path = if let Some(p) = image {
        p
    } else {
        let p = root.join("sample.jpg");
        // A small synthetic JPEG-ish payload.
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
        bytes.extend((0..4096).map(|i| (i % 256) as u8));
        std::fs::write(&p, &bytes)?;
        p
    };
    info("source file", image_path.display());
    let asset = ws
        .import_asset(album, &image_path)
        .map_err(|e| eyre!("import: {e}"))?;
    let st = ws.asset(&asset).ok_or_else(|| eyre!("asset missing"))?;
    let head = &st
        .chain
        .records()
        .last()
        .expect("provenance chain is never empty")
        .manifest;
    ok("encrypted with AES-256-GCM STREAM; manifest signed (device + write-tier hybrid sigs)");
    info("asset_id", asset);
    info("ciphertext hash", head.core.ciphertext_hash);
    info(
        "plaintext size",
        format!("{} bytes", head.core.plaintext_size),
    );
    ok("signed sidecar + provenance chain written to disk under media/");

    // ── 4. verify_asset chokepoint ───────────────────────────────────────────
    step(4, "Acknowledge via the verify_asset chokepoint");
    match ws.verify(&asset).map_err(|e| eyre!("verify: {e}"))? {
        VerifyOutcome::Accept => {
            ok("verify_asset → ACCEPT (both signatures, epoch, chain, AMK all valid)");
        }
        other => return Err(eyre!("unexpected verify outcome: {other:?}")),
    }

    // ── 5. CRDT metadata edits ───────────────────────────────────────────────
    step(5, "Collaborative metadata edits (CRDT, provenance-tracked)");
    ws.tag_add(&asset, "coast").map_err(|e| eyre!("tag: {e}"))?;
    ws.tag_add(&asset, "sunset")
        .map_err(|e| eyre!("tag: {e}"))?;
    ws.set_caption(&asset, "golden hour over the bay")
        .map_err(|e| eyre!("caption: {e}"))?;
    let st = ws.asset(&asset).expect("imported asset is present");
    let tags: Vec<String> = st.sidecar.tags_user.value().into_iter().collect();
    ok(format!("tags (OR-set): {tags:?}"));
    ok(format!(
        "caption (LWW): {:?}",
        st.sidecar.caption.get().cloned().unwrap_or_default()
    ));
    info("provenance records", st.chain.records().len());

    // ── 6. Lifecycle: soft delete + restore ──────────────────────────────────
    step(6, "Soft-delete (signed retention window) then restore");
    ws.soft_delete(&asset, 30)
        .map_err(|e| eyre!("delete: {e}"))?;
    ok("delete manifest signed with retention_until = now + 30 days");
    ws.restore(&asset).map_err(|e| eyre!("restore: {e}"))?;
    ok("trash-restore appended; the delete record is preserved in the chain (audit trail)");
    let st = ws.asset(&asset).expect("imported asset is present");
    let actions: Vec<String> = st
        .chain
        .records()
        .iter()
        .map(|r| format!("{:?}", r.manifest.core.action).to_lowercase())
        .collect();
    info("chain", actions.join(" → "));

    // ── 7. Backup export ─────────────────────────────────────────────────────
    step(7, "Export a portable, signed backup artifact");
    ws.export_backup(&backup_path, b"recovery-passphrase")
        .map_err(|e| eyre!("export: {e}"))?;
    let size = std::fs::metadata(&backup_path)?.len();
    ok("deterministic tar with HMAC + hybrid-signed MANIFEST + sealed AMK ledger");
    info(
        "backup",
        format!("{} ({} bytes)", backup_path.display(), size),
    );

    // ── 8. Restore into a fresh library ──────────────────────────────────────
    step(8, "Restore into a FRESH library and verify byte-equality");
    let exporter_pub = ws.exporter_verifying_key();
    let mut fresh = Workspace::create_with_params(&fresh_lib, b"new-device-pass", DEMO_KDF)
        .map_err(|e| eyre!("create fresh: {e}"))?;
    let added = fresh
        .import_backup(&backup_path, b"recovery-passphrase", &exporter_pub)
        .map_err(|e| eyre!("import backup: {e}"))?;
    ok(format!(
        "restored {added} asset(s) after verifying the exporter signature + AMK completeness"
    ));
    let original = ws
        .read_plaintext(&asset)
        .map_err(|e| eyre!("read src: {e}"))?;
    let restored = fresh
        .read_plaintext(&asset)
        .map_err(|e| eyre!("read restored: {e}"))?;
    if original == restored {
        ok(format!(
            "{} restored plaintext is byte-identical to the source",
            "PASS:".green().bold()
        ));
    } else {
        return Err(eyre!("restored plaintext differs from source"));
    }

    // A wrong exporter key (untrusted device) is refused.
    let bogus = Workspace::create_with_params(&root.join("bogus"), b"x", DEMO_KDF)
        .map_err(|e| eyre!("bogus ws: {e}"))?
        .exporter_verifying_key();
    let mut reject_lib = Workspace::create_with_params(&root.join("reject"), b"x", DEMO_KDF)
        .map_err(|e| eyre!("reject ws: {e}"))?;
    match reject_lib.import_backup(&backup_path, b"recovery-passphrase", &bogus) {
        Err(_) => ok("a backup signed by an untrusted exporter is refused"),
        Ok(_) => return Err(eyre!("untrusted exporter backup was wrongly accepted")),
    }

    // ── 9. Shamir social recovery ────────────────────────────────────────────
    step(9, "Opt-in Shamir 2-of-3 recovery-seed sharing");
    let seed = [0x5Au8; 32];
    let shares = split_seed_2of3(&seed);
    let recovered =
        recover_seed(&[shares[0].clone(), shares[2].clone()]).map_err(|e| eyre!("shamir: {e}"))?;
    if recovered == seed {
        ok("split into 3 shares; any 2 reconstruct the seed (1 alone reveals nothing)");
    } else {
        return Err(eyre!("shamir reconstruction failed"));
    }

    println!(
        "\n{}  Every layer of the design exercised offline with real cryptography.",
        "DONE.".green().bold()
    );
    println!(
        "{}",
        format!("Inspect the on-disk artifacts under {}", root.display()).dimmed()
    );
    Ok(())
}
