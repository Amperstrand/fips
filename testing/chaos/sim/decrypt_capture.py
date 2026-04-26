"""Post-run decryption of FIPS BLE traffic captures.

Reads btmon pcap captures + FIPS_NOISE_KEYLOG files produced during
chaos BLE scenarios and decrypts ChaCha20-Poly1305 FMP frames.

Wire format (FMP Established, phase 0x0):
  [0:1]  version_phase  (version=0, phase=0)
  [1:2]  flags          (K=0x01, CE=0x02, SP=0x04)
  [2:4]  payload_len    (u16 LE)
  [4:8]  receiver_idx   (u32 LE)
  [8:16] counter        (u64 LE)
  [16:]  ciphertext+tag (ChaCha20-Poly1305, 16-byte tag appended)

The 16-byte header is used as AAD. Counter maps to nonce as:
  nonce = [0x00; 4] || counter.to_le_bytes()  (12 bytes total)

Keylog format (FIPS_NOISE_KEYLOG):
  FIPS_LINK <local_npub> <peer_npub> <send_key_hex> <recv_key_hex>
  FIPS_SESSION <local_npub> <peer_npub> <send_key_hex> <recv_key_hex>

Usage:
  python3 -m sim.decrypt_capture <capture.log> <keys1.log> [keys2.log ...]
"""

from __future__ import annotations

import json
import logging
import os
import struct
import sys
from dataclasses import dataclass, field
from pathlib import Path

log = logging.getLogger(__name__)

COMMON_PREFIX_SIZE = 4
ESTABLISHED_HEADER_SIZE = 16
TAG_SIZE = 16

PHASE_ESTABLISHED = 0x0
PHASE_MSG1 = 0x1
PHASE_MSG2 = 0x2

FMP_MSG_TYPES = {
    0x00: "SessionDatagram",
    0x01: "SenderReport",
    0x02: "ReceiverReport",
    0x10: "TreeAnnounce",
    0x20: "FilterAnnounce",
    0x30: "LookupRequest",
    0x31: "LookupResponse",
    0x50: "Disconnect",
    0x51: "Heartbeat",
}


@dataclass
class LinkKeys:
    local_npub: str
    peer_npub: str
    send_key: bytes
    recv_key: bytes


@dataclass
class DecryptedFrame:
    index: int
    direction: str
    counter: int
    flags: int
    payload_len: int
    receiver_idx: int
    msg_type: str
    msg_type_hex: str
    inner_ts: int | None
    inner_len: int
    success: bool
    error: str | None = None


@dataclass
class DecryptResult:
    total_frames: int = 0
    decrypted_frames: int = 0
    failed_frames: int = 0
    handshake_msg1: int = 0
    handshake_msg2: int = 0
    unknown_phase: int = 0
    short_frames: int = 0
    msg_type_counts: dict[str, int] = field(default_factory=dict)
    frames: list[DecryptedFrame] = field(default_factory=list)
    keys_loaded: int = 0
    key_rekeys: int = 0

    def summary(self) -> str:
        lines = [
            "=== BLE Capture Decryption Analysis ===",
            "",
            f"Keys loaded:            {self.keys_loaded}",
            f"Rekey rotations:        {self.key_rekeys}",
            f"Total FMP frames:       {self.total_frames}",
            f"Decrypted:              {self.decrypted_frames}",
            f"Decryption failures:    {self.failed_frames}",
            f"Handshake msg1:         {self.handshake_msg1}",
            f"Handshake msg2:         {self.handshake_msg2}",
            f"Short/truncated:        {self.short_frames}",
            f"Unknown phase:          {self.unknown_phase}",
            "",
            "--- Message Types ---",
        ]
        for msg_type, count in sorted(self.msg_type_counts.items()):
            lines.append(f"  {msg_type}: {count}")
        lines.append("")
        return "\n".join(lines)

    def to_dict(self) -> dict:
        return {
            "keys_loaded": self.keys_loaded,
            "key_rekeys": self.key_rekey_count,
            "total_frames": self.total_frames,
            "decrypted_frames": self.decrypted_frames,
            "failed_frames": self.failed_frames,
            "handshake_msg1": self.handshake_msg1,
            "handshake_msg2": self.handshake_msg2,
            "msg_type_counts": self.msg_type_counts,
        }


def parse_keylog_files(paths: list[str]) -> list[LinkKeys]:
    keys = []
    for path in paths:
        if not os.path.exists(path):
            log.warning("Keylog file not found: %s", path)
            continue
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                parts = line.split()
                if len(parts) < 5:
                    continue
                label, local, peer, send_hex, recv_hex = parts[0], parts[1], parts[2], parts[3], parts[4]
                if label == "FIPS_LINK":
                    keys.append(LinkKeys(
                        local_npub=local,
                        peer_npub=peer,
                        send_key=bytes.fromhex(send_hex),
                        recv_key=bytes.fromhex(recv_hex),
                    ))
    return keys


def counter_to_nonce(counter: int) -> bytes:
    nonce = bytearray(12)
    nonce[4:12] = counter.to_bytes(8, "little")
    return bytes(nonce)


def decrypt_frame(
    ciphertext: bytes,
    counter: int,
    aad: bytes,
    key: bytes,
) -> bytes | None:
    try:
        from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
    except ImportError:
        log.error(
            "cryptography package not installed. "
            "Install with: pip install cryptography"
        )
        return None

    cipher = ChaCha20Poly1305(key)
    nonce = counter_to_nonce(counter)
    try:
        return cipher.decrypt(nonce, ciphertext, aad)
    except Exception:
        return None


def parse_inner_payload(plaintext: bytes) -> tuple[str, int | None, int]:
    if len(plaintext) < 1:
        return "Unknown(0x??)", None, 0

    msg_type = plaintext[0]
    msg_name = FMP_MSG_TYPES.get(msg_type, f"Unknown(0x{msg_type:02x})")

    inner_ts = None
    if len(plaintext) >= 5:
        inner_ts = struct.unpack_from("<I", plaintext, 1)[0]

    return msg_name, inner_ts, len(plaintext)


def decrypt_capture(
    capture_path: str,
    keylog_paths: list[str],
) -> DecryptResult:
    result = DecryptResult()

    all_keys = parse_keylog_files(keylog_paths)
    result.keys_loaded = len(all_keys)

    if not all_keys:
        log.warning("No link keys found — cannot decrypt")
        result.error = "no_keys"
        return result

    latest_keys = all_keys[-1] if all_keys else None
    result.key_rekeys = max(0, len(all_keys) - 1)

    if not os.path.exists(capture_path):
        log.warning("Capture file not found: %s", capture_path)
        result.error = "no_capture"
        return result

    try:
        frames = _read_btmon_btsnoop(capture_path)
    except Exception:
        try:
            frames = _read_raw_frames(capture_path)
        except Exception as e:
            log.error("Failed to parse capture: %s", e)
            result.error = f"parse_error: {e}"
            return result

    for idx, raw_frame in enumerate(frames):
        result.total_frames += 1
        frame = _process_frame(idx, raw_frame, latest_keys)
        result.frames.append(frame)

        if frame.success:
            result.decrypted_frames += 1
            result.msg_type_counts[frame.msg_type] = result.msg_type_counts.get(frame.msg_type, 0) + 1
        elif frame.msg_type == "Handshake":
            pass
        else:
            result.failed_frames += 1

    return result


def _process_frame(idx: int, raw: bytes, keys: LinkKeys | None) -> DecryptedFrame:
    if len(raw) < COMMON_PREFIX_SIZE:
        return DecryptedFrame(
            index=idx, direction="?", counter=0, flags=0,
            payload_len=0, receiver_idx=0, msg_type="ShortFrame",
            msg_type_hex="0x??", inner_ts=None, inner_len=0,
            success=False, error="too_short",
        )

    vp = raw[0]
    phase = vp & 0x0F
    flags = raw[1]
    payload_len = struct.unpack_from("<H", raw, 2)[0]

    if phase == PHASE_MSG1:
        return DecryptedFrame(
            index=idx, direction="->", counter=0, flags=flags,
            payload_len=payload_len, receiver_idx=0,
            msg_type="Handshake", msg_type_hex="msg1",
            inner_ts=None, inner_len=0, success=False,
        )

    if phase == PHASE_MSG2:
        return DecryptedFrame(
            index=idx, direction="<-", counter=0, flags=flags,
            payload_len=payload_len, receiver_idx=0,
            msg_type="Handshake", msg_type_hex="msg2",
            inner_ts=None, inner_len=0, success=False,
        )

    if phase != PHASE_ESTABLISHED:
        return DecryptedFrame(
            index=idx, direction="?", counter=0, flags=flags,
            payload_len=payload_len, receiver_idx=0,
            msg_type="UnknownPhase", msg_type_hex=f"0x{phase:02x}",
            inner_ts=None, inner_len=0, success=False,
            error=f"unknown_phase_{phase}",
        )

    if len(raw) < ESTABLISHED_HEADER_SIZE:
        return DecryptedFrame(
            index=idx, direction="?", counter=0, flags=flags,
            payload_len=payload_len, receiver_idx=0,
            msg_type="ShortFrame", msg_type_hex="0x??",
            inner_ts=None, inner_len=0, success=False,
            error="header_too_short",
        )

    receiver_idx = struct.unpack_from("<I", raw, 4)[0]
    counter = struct.unpack_from("<Q", raw, 8)[0]

    ciphertext = raw[ESTABLISHED_HEADER_SIZE:]
    aad = raw[:ESTABLISHED_HEADER_SIZE]

    if keys is None or len(ciphertext) < TAG_SIZE:
        return DecryptedFrame(
            index=idx, direction="?", counter=counter, flags=flags,
            payload_len=payload_len, receiver_idx=receiver_idx,
            msg_type="NoKey", msg_type_hex="0x??",
            inner_ts=None, inner_len=0, success=False,
            error="no_keys" if keys is None else "ciphertext_too_short",
        )

    plaintext = decrypt_frame(ciphertext, counter, aad, keys.recv_key)
    direction = "<-"
    if plaintext is None:
        plaintext = decrypt_frame(ciphertext, counter, aad, keys.send_key)
        direction = "->"

    if plaintext is None:
        return DecryptedFrame(
            index=idx, direction="?", counter=counter, flags=flags,
            payload_len=payload_len, receiver_idx=receiver_idx,
            msg_type="DecryptFail", msg_type_hex="0x??",
            inner_ts=None, inner_len=0, success=False,
            error="decrypt_failed",
        )

    msg_name, inner_ts, inner_len = parse_inner_payload(plaintext)
    msg_hex = f"0x{plaintext[0]:02x}" if plaintext else "0x??"

    return DecryptedFrame(
        index=idx, direction=direction, counter=counter, flags=flags,
        payload_len=payload_len, receiver_idx=receiver_idx,
        msg_type=msg_name, msg_type_hex=msg_hex,
        inner_ts=inner_ts, inner_len=inner_len,
        success=True,
    )


def _read_btmon_btsnoop(path: str) -> list[bytes]:
    with open(path, "rb") as f:
        magic = f.read(8)
        if magic != b"btsnoop\x00":
            raise ValueError("Not a btsnoop file")

        f.read(4 + 4)

        frames = []
        while True:
            hdr = f.read(24)
            if len(hdr) < 24:
                break
            _orig_len, _flags, _drops, _ts = struct.unpack(">IIII", hdr)
            data_len = struct.unpack(">I", f.read(4))[0]
            f.read(4)
            data = f.read(data_len)
            if len(data) < 9:
                continue

            payload = _extract_l2cap_payload(data)
            if payload is not None:
                frames.append(payload)

        return frames


def _extract_l2cap_payload(data: bytes) -> bytes | None:
    if len(data) < 9:
        return None

    hci_type = data[0] if len(data) > 0 else 0

    if hci_type == 0x02:
        if len(data) < 7:
            return None
        handle = struct.unpack_from("<H", data, 1)[0] & 0x0FFF
        acl_len = struct.unpack_from("<H", data, 3)[0]
        if len(data) < 5 + acl_len:
            return None
        l2cap = data[5:5 + acl_len]
        if len(l2cap) < 4:
            return None
        psm = struct.unpack_from("<H", l2cap, 0)[0] | (
            (struct.unpack_from("<H", l2cap, 2)[0] & 0xFF00)
        )
        cid = struct.unpack_from("<H", l2cap, 2)[0]
        if cid == 0x0040 + 0x0085 - 0x0080:
            return l2cap[4:]
        return None

    if hci_type == 0x02:
        pass

    return None


def _read_raw_frames(path: str) -> list[bytes]:
    frames = []
    with open(path, "rb") as f:
        data = f.read()

    offset = 0
    while offset < len(data):
        if offset + 4 > len(data):
            break

        vp = data[offset]
        phase = vp & 0x0F

        if phase > 0x3 or (vp >> 4) > 0:
            offset += 1
            continue

        payload_len = struct.unpack_from("<H", data, offset + 2)[0]

        if phase == PHASE_ESTABLISHED:
            frame_len = ESTABLISHED_HEADER_SIZE + TAG_SIZE + max(0, payload_len - ESTABLISHED_HEADER_SIZE - TAG_SIZE)
            frame_len = max(ESTABLISHED_HEADER_SIZE + TAG_SIZE, payload_len + COMMON_PREFIX_SIZE)
        elif phase == PHASE_MSG1:
            frame_len = 114
        elif phase == PHASE_MSG2:
            frame_len = 69
        else:
            offset += 1
            continue

        if offset + frame_len > len(data):
            break

        frames.append(data[offset:offset + frame_len])
        offset += frame_len

    return frames


def main():
    if len(sys.argv) < 3:
        print(
            f"Usage: {sys.argv[0]} <capture.log> <keys1.log> [keys2.log ...]",
            file=sys.stderr,
        )
        print(
            f"       {sys.argv[0]} --output-dir <dir>   (auto-discover files)",
            file=sys.stderr,
        )
        sys.exit(1)

    logging.basicConfig(level=logging.INFO, format="%(levelname)s: %(message)s")

    if sys.argv[1] == "--output-dir":
        output_dir = sys.argv[2]
        capture_path = os.path.join(output_dir, "ble-capture-hci0.log")
        keylog_paths = sorted(
            str(p) for p in Path(output_dir).glob("keys-*.log")
        )
    else:
        capture_path = sys.argv[1]
        keylog_paths = sys.argv[2:]

    result = decrypt_capture(capture_path, keylog_paths)
    print(result.summary())

    json_path = capture_path.rsplit(".", 1)[0] + "-decrypt.json"
    with open(json_path, "w") as f:
        json.dump(
            {
                "capture": capture_path,
                "keylogs": keylog_paths,
                "total_frames": result.total_frames,
                "decrypted_frames": result.decrypted_frames,
                "failed_frames": result.failed_frames,
                "msg_type_counts": result.msg_type_counts,
                "frames": [
                    {
                        "idx": fr.index,
                        "dir": fr.direction,
                        "counter": fr.counter,
                        "msg_type": fr.msg_type,
                        "inner_ts": fr.inner_ts,
                        "success": fr.success,
                    }
                    for fr in result.frames
                    if fr.success
                ],
            },
            f,
            indent=2,
        )
    print(f"Decrypted frame details written to {json_path}")


if __name__ == "__main__":
    main()
