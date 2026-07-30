use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};

const USAGE: &str = "\
usage: sigil-verify-tauri-update \
--config PATH --archive PATH --signature PATH";

struct VerificationInput {
    config: PathBuf,
    archive: PathBuf,
    signature: PathBuf,
}

fn main() -> ExitCode {
    let input = match parse_args(env::args_os().skip(1)) {
        Ok(input) => input,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let (public_key, signature) = match load_verification_material(&input) {
        Ok(material) => material,
        Err(error) => {
            eprintln!("unable to load Tauri updater verification material: {error}");
            return ExitCode::from(2);
        }
    };
    let archive = match read_regular_file(&input.archive) {
        Ok(archive) => archive,
        Err(error) => {
            eprintln!(
                "unable to read updater archive {}: {error}",
                input.archive.display()
            );
            return ExitCode::from(2);
        }
    };

    match public_key.verify(&archive, &signature, true) {
        Ok(()) => {
            println!(
                "verified Tauri updater signature: {}",
                input.archive.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "Tauri updater signature verification failed for {}: {error}",
                input.archive.display()
            );
            ExitCode::from(1)
        }
    }
}

fn parse_args(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<VerificationInput, String> {
    let mut config = None;
    let mut archive = None;
    let mut signature = None;

    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {}", flag.to_string_lossy()))?;
        match flag.to_str() {
            Some("--config") => set_once(&mut config, value.into(), "--config")?,
            Some("--archive") => set_once(&mut archive, value.into(), "--archive")?,
            Some("--signature") => set_once(&mut signature, value.into(), "--signature")?,
            _ => return Err(format!("unknown argument: {}", flag.to_string_lossy())),
        }
    }

    Ok(VerificationInput {
        config: config.ok_or_else(|| "missing --config".to_owned())?,
        archive: archive.ok_or_else(|| "missing --archive".to_owned())?,
        signature: signature.ok_or_else(|| "missing --signature".to_owned())?,
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{flag} may only be supplied once"));
    }
    Ok(())
}

fn load_verification_material(
    input: &VerificationInput,
) -> Result<(PublicKey, Signature), Box<dyn std::error::Error>> {
    let config_bytes = read_regular_file(&input.config)?;
    let config: serde_json::Value = serde_json::from_slice(&config_bytes)?;
    let encoded_public_key = config
        .pointer("/plugins/updater/pubkey")
        .and_then(serde_json::Value::as_str)
        .ok_or("Tauri config is missing plugins.updater.pubkey")?;
    let public_key_text = decode_base64_utf8(encoded_public_key, &input.config)?;
    let public_key = PublicKey::decode(&public_key_text)?;

    let signature_bytes = read_regular_file(&input.signature)?;
    if signature_bytes.len() > 4_096 {
        return Err("updater signature exceeds 4096 bytes".into());
    }
    let encoded_signature = std::str::from_utf8(&signature_bytes)?.trim();
    if encoded_signature.is_empty() {
        return Err("updater signature is empty".into());
    }
    let signature_text = decode_base64_utf8(encoded_signature, &input.signature)?;
    let signature = Signature::decode(&signature_text)?;

    Ok((public_key, signature))
}

fn read_regular_file(path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("expected a non-symlink regular file: {}", path.display()).into());
    }
    Ok(fs::read(path)?)
}

fn decode_base64_utf8(encoded: &str, source: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|error| format!("invalid base64 in {}: {error}", source.display()))?;
    String::from_utf8(decoded)
        .map_err(|error| format!("invalid UTF-8 in {}: {error}", source.display()).into())
}
