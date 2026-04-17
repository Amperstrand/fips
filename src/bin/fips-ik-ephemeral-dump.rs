use clap::{Parser, Subcommand};
use fips::identity::Identity;
use fips::node::wire::{strip_inner_header, EncryptedHeader, Msg1Header, Msg2Header};
use fips::noise::{HandshakeState, IkDebugRecord, NoiseSession};
use fips::protocol::{LinkMessageType, SessionMessageType};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "fips-ik-ephemeral-dump")]
#[command(about = "Reconstruct and decrypt FIPS Noise IK BLE captures from debug key logs")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    DecryptFmp {
        #[arg(long)]
        nsec: String,
        #[arg(long)]
        key_log: PathBuf,
        #[arg(long)]
        frames_json: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
struct ExportedFrame {
    #[serde(default)]
    frame_number: Option<u64>,
    #[serde(default)]
    time_epoch: Option<String>,
    payload_hex: String,
}

fn parse_debug_record(path: &PathBuf) -> Result<IkDebugRecord, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read key log: {e}"))?;
    let mut msg2_record = None;
    let mut fallback = None;

    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let event = IkDebugRecord::event_from_json_line(line);
        let record =
            IkDebugRecord::from_json_line(line).map_err(|e| format!("parse key log line: {e}"))?;
        if fallback.is_none() {
            fallback = Some(record.clone());
        }
        if event.as_deref() == Some("ik_msg2_write") {
            msg2_record = Some(record);
        }
    }

    msg2_record
        .or(fallback)
        .ok_or_else(|| "no IK debug records found".to_string())
}

fn parse_frames(path: &PathBuf) -> Result<Vec<ExportedFrame>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read frames: {e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("parse frames json: {e}"))
}

fn decode_link_message_name(byte: u8) -> String {
    LinkMessageType::from_byte(byte)
        .map(|m| m.to_string())
        .unwrap_or_else(|| format!("Unknown(0x{byte:02x})"))
}

fn decode_session_message_name(byte: u8) -> String {
    SessionMessageType::from_byte(byte)
        .map(|m| m.to_string())
        .unwrap_or_else(|| format!("Unknown(0x{byte:02x})"))
}

fn decrypt_fmp(nsec: String, key_log: PathBuf, frames_json: PathBuf) -> Result<(), String> {
    let identity = Identity::from_secret_str(&nsec).map_err(|e| format!("parse nsec: {e}"))?;
    let debug_record = parse_debug_record(&key_log)?;
    let mut session: NoiseSession =
        HandshakeState::reconstruct_ik_session_from_debug(identity.keypair(), &debug_record)
            .map_err(|e| format!("reconstruct IK session: {e}"))?;
    let frames = parse_frames(&frames_json)?;

    for frame in frames {
        let payload =
            hex::decode(&frame.payload_hex).map_err(|e| format!("decode payload hex: {e}"))?;

        if let Some(header) = EncryptedHeader::parse(&payload) {
            let ciphertext = &payload[header.ciphertext_offset()..];
            match session.decrypt_with_replay_check_and_aad(
                ciphertext,
                header.counter,
                &header.header_bytes,
            ) {
                Ok(plaintext) => {
                    if let Some((timestamp, link_payload)) = strip_inner_header(&plaintext) {
                        let msg_type = link_payload.first().copied().unwrap_or(0xff);
                        let link_name = decode_link_message_name(msg_type);
                        let session_name = if msg_type == LinkMessageType::SessionDatagram.to_byte()
                        {
                            if link_payload.len() > 1 + 1 + 2 + 16 + 16 {
                                let offset = 1 + 1 + 2 + 16 + 16;
                                decode_session_message_name(link_payload[offset])
                            } else {
                                "SessionDatagram(truncated)".to_string()
                            }
                        } else {
                            "".to_string()
                        };

                        println!(
                            "frame={} time={} counter={} receiver_idx={} timestamp={} link_type={} session_type={} plaintext_hex={}",
                            frame.frame_number.unwrap_or(0),
                            frame.time_epoch.unwrap_or_default(),
                            header.counter,
                            header.receiver_idx,
                            timestamp,
                            link_name,
                            session_name,
                            hex::encode(&plaintext),
                        );
                    } else {
                        println!(
                            "frame={} time={} counter={} receiver_idx={} plaintext_hex={}",
                            frame.frame_number.unwrap_or(0),
                            frame.time_epoch.unwrap_or_default(),
                            header.counter,
                            header.receiver_idx,
                            hex::encode(&plaintext),
                        );
                    }
                }
                Err(error) => {
                    eprintln!(
                        "frame={} time={} counter={} decrypt_error={}",
                        frame.frame_number.unwrap_or(0),
                        frame.time_epoch.unwrap_or_default(),
                        header.counter,
                        error,
                    );
                }
            }
        } else if let Some(header) = Msg1Header::parse(&payload) {
            let noise_msg = &payload[header.noise_msg1_offset..];
            println!(
                "frame={} time={} type=NoiseIKMsg1 sender_idx={} msg_hex={}",
                frame.frame_number.unwrap_or(0),
                frame.time_epoch.unwrap_or_default(),
                header.sender_idx,
                hex::encode(noise_msg),
            );
        } else if let Some(header) = Msg2Header::parse(&payload) {
            let noise_msg = &payload[header.noise_msg2_offset..];
            println!(
                "frame={} time={} type=NoiseIKMsg2 sender_idx={} receiver_idx={} msg_hex={}",
                frame.frame_number.unwrap_or(0),
                frame.time_epoch.unwrap_or_default(),
                header.sender_idx,
                header.receiver_idx,
                hex::encode(noise_msg),
            );
        }
    }

    Ok(())
}

fn main() {
    let args = Args::parse();

    let result = match args.command {
        Command::DecryptFmp {
            nsec,
            key_log,
            frames_json,
        } => decrypt_fmp(nsec, key_log, frames_json),
    };

    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
