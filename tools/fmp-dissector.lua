--[[==========================================================================
  fmp-dissector.lua — Wireshark Lua dissector for FIPS Mesh Protocol (FMP)

  Dissects FMP frames carried over Bluetooth L2CAP CoC on PSM 133.
  Handles BLE transport framing, the 4-byte FMP common prefix, and all
  three session lifecycle phases (Established, Noise IK msg1, Noise IK msg2).

  INSTALLATION
    Option A — system plugins:
      cp fmp-dissector.lua ~/.local/lib/wireshark/plugins/
      (or ~/.wireshark/plugins/ on older setups)

    Option B — ad-hoc (no install):
      tshark -r capture.btsnoop -X lua_script:fmp-dissector.lua
      wireshark -r capture.btsnoop -X lua_script:fmp-dissector.lua

  USAGE
    The dissector auto-registers on L2CAP PSM 133.  Wireshark must decode
    the capture through its btsnoop → HCI → ACL → L2CAP stack first.
    FMP frames will appear under the L2CAP layer in the packet detail tree.

  LIMITATIONS
    AEAD-encrypted payloads are displayed as raw bytes.  Decryption requires
    key material from a running FIPS node and is not yet implemented.

  WIRE FORMAT REFERENCE
    See docs/reference/wire-formats.md in the FIPS repository.

  Copyright 2025 — MIT License.
--]]

---------------------------------------------------------------------------
-- Protocol declaration
---------------------------------------------------------------------------
local fmp_proto = Proto.new("fips_mesh", "FIPS Mesh Protocol")

---------------------------------------------------------------------------
-- Constants
---------------------------------------------------------------------------
local FMP_PSM = 133

local PHASE_ESTABLISHED = 0x0
local PHASE_MSG1        = 0x1
local PHASE_MSG2        = 0x2

local COMMON_PREFIX_SIZE     = 4
local ESTABLISHED_HDR_SIZE   = 16   -- common_prefix + receiver_idx + counter
local INNER_HDR_SIZE         = 5    -- u32 timestamp + u8 msg_type (after decryption)
local AEAD_TAG_SIZE          = 16

local NOISE_MSG1_SIZE        = 106  -- ephemeral(33) + enc_static(49) + enc_epoch(24)
local NOISE_MSG2_SIZE        = 57   -- ephemeral(33) + enc_epoch(24)

---------------------------------------------------------------------------
-- Link-layer message type name table (inside encrypted Established frames)
-- Displayed as contextual info; actual dissection requires decryption.
---------------------------------------------------------------------------
local LINK_MSG_NAMES = {
    [0x00] = "SessionDatagram",
    [0x01] = "SenderReport",
    [0x02] = "ReceiverReport",
    [0x10] = "TreeAnnounce",
    [0x20] = "FilterAnnounce",
    [0x30] = "LookupRequest",
    [0x31] = "LookupResponse",
    [0x50] = "Disconnect",
    [0x51] = "Heartbeat",
}

---------------------------------------------------------------------------
-- Phase name table
---------------------------------------------------------------------------
local PHASE_NAMES = {
    [0x0] = "Established",
    [0x1] = "Noise IK msg1 (handshake initiation)",
    [0x2] = "Noise IK msg2 (handshake response)",
}

---------------------------------------------------------------------------
-- Protocol fields
---------------------------------------------------------------------------

-- Common prefix
local hf_version     = ProtoField.new("Version",         "fmp.version",     ftypes.UINT8, nil, base.DEC, 0xF0)
local hf_phase       = ProtoField.new("Phase",           "fmp.phase",       ftypes.UINT8, nil, base.DEC, 0x0F)
local hf_flags_byte  = ProtoField.new("Flags",           "fmp.flags",       ftypes.UINT8)
local hf_payload_len = ProtoField.new("Payload Length",  "fmp.payload_len", ftypes.UINT16, nil, base.DEC)

-- Established-phase flag bits
local hf_flag_k  = ProtoField.new("K — Key Epoch",             "fmp.flags.k",  ftypes.BOOLEAN)
local hf_flag_ce = ProtoField.new("CE — Congestion Experienced", "fmp.flags.ce", ftypes.BOOLEAN)
local hf_flag_sp = ProtoField.new("SP — Spin Bit",             "fmp.flags.sp", ftypes.BOOLEAN)

-- Established outer header
local hf_receiver_idx = ProtoField.new("Receiver Index", "fmp.receiver_idx", ftypes.UINT32, nil, base.DEC)
local hf_counter      = ProtoField.new("Counter",        "fmp.counter",      ftypes.UINT64, nil, base.DEC)

-- Established encrypted payload
local hf_encrypted_payload = ProtoField.new("Encrypted Payload", "fmp.encrypted_payload", ftypes.BYTES)
local hf_aead_tag          = ProtoField.new("AEAD Tag",          "fmp.aead_tag",          ftypes.BYTES)

-- Handshake common
local hf_sender_idx = ProtoField.new("Sender Index", "fmp.sender_idx", ftypes.UINT32, nil, base.DEC)

-- Handshake msg1 fields
local hf_ephemeral_pubkey  = ProtoField.new("Ephemeral Public Key",  "fmp.ephemeral_pubkey",  ftypes.BYTES)
local hf_encrypted_static  = ProtoField.new("Encrypted Static Key",  "fmp.encrypted_static",  ftypes.BYTES)
local hf_encrypted_epoch   = ProtoField.new("Encrypted Epoch",       "fmp.encrypted_epoch",   ftypes.BYTES)

-- BLE transport framing
local hf_sdu_length    = ProtoField.new("SDU Length",    "fmp.sdu_length",    ftypes.UINT16, nil, base.DEC)
local hf_frame_length  = ProtoField.new("Frame Length",  "fmp.frame_length",  ftypes.UINT16, nil, base.DEC)

-- Msg type hint (only visible if decrypted externally / heuristic)
local hf_msg_type_hint = ProtoField.new("Message Type (if decrypted)", "fmp.msg_type_hint", ftypes.STRING)

fmp_proto.fields = {
    hf_version, hf_phase, hf_flags_byte, hf_payload_len,
    hf_flag_k, hf_flag_ce, hf_flag_sp,
    hf_receiver_idx, hf_counter,
    hf_encrypted_payload, hf_aead_tag,
    hf_sender_idx,
    hf_ephemeral_pubkey, hf_encrypted_static, hf_encrypted_epoch,
    hf_sdu_length, hf_frame_length,
    hf_msg_type_hint,
}

---------------------------------------------------------------------------
-- Subtree helpers
---------------------------------------------------------------------------

--- Dissect the 4-byte common prefix and return (version, phase, flags, payload_len).
local function dissect_common_prefix(buf, pinfo, tree)
    local subtree = tree:add(fmp_proto, buf(0, COMMON_PREFIX_SIZE), "FMP Common Prefix")

    local ver_phase = buf(0, 1)
    subtree:add(hf_version, ver_phase)
    subtree:add(hf_phase, ver_phase)

    local flags_byte = buf(1, 1)
    subtree:add(hf_flags_byte, flags_byte)

    local payload_len = buf(2, 2):le_uint()
    subtree:add_le(hf_payload_len, buf(2, 2))

    local version = bit.rshift(bit.band(ver_phase:uint(), 0xF0), 4)
    local phase   = bit.band(ver_phase:uint(), 0x0F)
    local flags   = flags_byte:uint()

    return version, phase, flags, payload_len
end

--- Add flag-bit fields under an Established frame.
local function dissect_established_flags(buf, tree)
    local subtree = tree:add(fmp_proto, buf, "Flags")
    subtree:add(hf_flag_k,  buf)
    subtree:add(hf_flag_ce, buf)
    subtree:add(hf_flag_sp, buf)
end

--- Dissect a single FMP frame starting at the given offset inside a tvb.
-- Returns number of bytes consumed, or 0 on error.
local function dissect_fmp_frame(buf, pinfo, root_tree)
    local frame_len = buf:len()
    if frame_len < COMMON_PREFIX_SIZE then
        return 0
    end

    -- Common prefix
    local version, phase, flags, payload_len = dissect_common_prefix(buf, pinfo, root_tree)

    -- Validate version
    if version ~= 0 then
        root_tree:add_proto_expert_info(PI_ERROR, PI_MALFORMED,
            string.format("Unknown FMP version %d (expected 0)", version)
        )
    end

    local phase_name = PHASE_NAMES[phase] or string.format("Unknown (0x%x)", phase)

    -- Set Info column
    if phase == PHASE_ESTABLISHED then
        pinfo.cols.info = string.format("FMP Established len=%d flags=0x%02x", payload_len, flags)
    elseif phase == PHASE_MSG1 then
        pinfo.cols.info = string.format("FMP Noise IK msg1 len=%d", payload_len)
    elseif phase == PHASE_MSG2 then
        pinfo.cols.info = string.format("FMP Noise IK msg2 len=%d", payload_len)
    else
        pinfo.cols.info = string.format("FMP Unknown phase 0x%x len=%d", phase, payload_len)
    end

    -- Phase-specific dissection
    if phase == PHASE_ESTABLISHED then
        ---------------------------------------------------------------
        -- Established frame (phase 0x0)
        ---------------------------------------------------------------
        if frame_len < ESTABLISHED_HDR_SIZE then
            root_tree:add_proto_expert_info(
                fmp_proto.expert_info.truncated,
                string.format("Established header truncated: %d bytes (need %d)", frame_len, ESTABLISHED_HDR_SIZE)
            )
            return frame_len
        end

        local hdr_tree = root_tree:add(fmp_proto, buf(0, ESTABLISHED_HDR_SIZE),
            "FMP Established Outer Header (16 bytes, AEAD AAD)")

        -- Re-add common prefix under header subtree for clarity
        dissect_common_prefix(buf(0, COMMON_PREFIX_SIZE), pinfo, hdr_tree)

        -- Flag bits
        local flags_buf = buf(1, 1)
        local flags_sub = hdr_tree:add(fmp_proto, flags_buf, "Flags")
        flags_sub:add(hf_flag_k,  flags_buf)
        flags_sub:add(hf_flag_ce, flags_buf)
        flags_sub:add(hf_flag_sp, flags_buf)

        hdr_tree:add_le(hf_receiver_idx, buf(4, 4))
        hdr_tree:add_le(hf_counter, buf(8, 8))

        -- Encrypted payload (payload_len bytes) + AEAD tag (16 bytes)
        local enc_start  = ESTABLISHED_HDR_SIZE
        local enc_end    = enc_start + payload_len
        local tag_start  = enc_end
        local tag_end    = tag_start + AEAD_TAG_SIZE

        if tag_end > frame_len then
            -- Truncated: show what we have
            if enc_start < frame_len then
                hdr_tree:add(fmp_proto, buf(enc_start, frame_len - enc_start),
                    string.format("Encrypted Payload (truncated, %d bytes)", frame_len - enc_start))
            end
            return frame_len
        end

        if payload_len > 0 then
            local enc_tree = root_tree:add(fmp_proto, buf(enc_start, payload_len), "Encrypted Payload")
            enc_tree:add(hf_encrypted_payload, buf(enc_start, payload_len))
        end

        local tag_tree = root_tree:add(fmp_proto, buf(tag_start, AEAD_TAG_SIZE), "AEAD Tag (Poly1305)")
        tag_tree:add(hf_aead_tag, buf(tag_start, AEAD_TAG_SIZE))

        -- Heuristic: if encrypted payload starts with a recognizable inner header
        -- after external decryption, a tap or post-dissector could annotate this.
        -- For now, note the total wire size for reference.
        local total_wire = tag_end
        root_tree:add(fmp_proto, buf(0, 0),
            string.format("Total frame: %d bytes (header %d + ciphertext %d + tag %d)",
                total_wire, ESTABLISHED_HDR_SIZE, payload_len, AEAD_TAG_SIZE)):set_generated()

        return tag_end

    elseif phase == PHASE_MSG1 then
        ---------------------------------------------------------------
        -- Noise IK msg1 (phase 0x1)
        ---------------------------------------------------------------
        -- Wire: common_prefix(4) + sender_idx(4) + noise_msg1(106)
        local expected_len = COMMON_PREFIX_SIZE + 4 + NOISE_MSG1_SIZE   -- 114
        local actual_len   = math.min(frame_len, expected_len)

        local msg_tree = root_tree:add(fmp_proto, buf(0, actual_len),
            string.format("FMP Noise IK msg1 (%d bytes)", actual_len))

        -- Re-add common prefix
        dissect_common_prefix(buf(0, COMMON_PREFIX_SIZE), pinfo, msg_tree)

        if frame_len < COMMON_PREFIX_SIZE + 4 then
            msg_tree:add_proto_expert_info(
                fmp_proto.expert_info.truncated,
                "Handshake msg1 truncated: missing sender_idx"
            )
            return frame_len
        end

        msg_tree:add_le(hf_sender_idx, buf(4, 4))

        if frame_len < COMMON_PREFIX_SIZE + 4 + NOISE_MSG1_SIZE then
            msg_tree:add_proto_expert_info(
                fmp_proto.expert_info.truncated,
                string.format("Handshake msg1 truncated: need %d bytes, have %d",
                    expected_len, frame_len)
            )
            return frame_len
        end

        -- Noise msg1 breakdown
        local noise_tree = msg_tree:add(fmp_proto, buf(8, NOISE_MSG1_SIZE), "Noise IK msg1 Payload")

        noise_tree:add(hf_ephemeral_pubkey, buf(8, 33))
        noise_tree:add(hf_encrypted_static, buf(41, 49))  -- 8 + 33 = 41
        noise_tree:add(hf_encrypted_epoch,  buf(90, 24))  -- 41 + 49 = 90

        pinfo.cols.info = string.format("FMP Noise IK msg1 sender_idx=%d", buf(4, 4):le_uint())

        return expected_len

    elseif phase == PHASE_MSG2 then
        ---------------------------------------------------------------
        -- Noise IK msg2 (phase 0x2)
        ---------------------------------------------------------------
        -- Wire: common_prefix(4) + sender_idx(4) + receiver_idx(4) + noise_msg2(57)
        local expected_len = COMMON_PREFIX_SIZE + 4 + 4 + NOISE_MSG2_SIZE  -- 69
        local actual_len   = math.min(frame_len, expected_len)

        local msg_tree = root_tree:add(fmp_proto, buf(0, actual_len),
            string.format("FMP Noise IK msg2 (%d bytes)", actual_len))

        -- Re-add common prefix
        dissect_common_prefix(buf(0, COMMON_PREFIX_SIZE), pinfo, msg_tree)

        if frame_len < COMMON_PREFIX_SIZE + 4 + 4 then
            msg_tree:add_proto_expert_info(
                fmp_proto.expert_info.truncated,
                "Handshake msg2 truncated: missing sender_idx/receiver_idx"
            )
            return frame_len
        end

        msg_tree:add_le(hf_sender_idx,   buf(4, 4))
        msg_tree:add_le(hf_receiver_idx, buf(8, 4))

        if frame_len < expected_len then
            msg_tree:add_proto_expert_info(
                fmp_proto.expert_info.truncated,
                string.format("Handshake msg2 truncated: need %d bytes, have %d",
                    expected_len, frame_len)
            )
            return frame_len
        end

        -- Noise msg2 breakdown
        local noise_tree = msg_tree:add(fmp_proto, buf(12, NOISE_MSG2_SIZE), "Noise IK msg2 Payload")

        noise_tree:add(hf_ephemeral_pubkey, buf(12, 33))
        noise_tree:add(hf_encrypted_epoch,  buf(45, 24))  -- 12 + 33 = 45

        pinfo.cols.info = string.format("FMP Noise IK msg2 sender_idx=%d receiver_idx=%d",
            buf(4, 4):le_uint(), buf(8, 4):le_uint())

        return expected_len

    else
        ---------------------------------------------------------------
        -- Unknown phase
        ---------------------------------------------------------------
        root_tree:add_proto_expert_info(PI_WARN, PI_PROTOCOL,
            string.format("Unknown FMP phase 0x%x", phase)
        )

        -- Show remaining bytes as raw data
        root_tree:add(fmp_proto, buf(COMMON_PREFIX_SIZE),
            string.format("Unknown payload (%d bytes)", frame_len - COMMON_PREFIX_SIZE))

        return frame_len
    end
end

---------------------------------------------------------------------------
-- Main dissector entry point
---------------------------------------------------------------------------
function fmp_proto.dissector(tvb, pinfo, tree)
    local buf_len = tvb:len()
    if buf_len < 2 then
        return 0
    end

    pinfo.cols.protocol = "FMP"

    -----------------------------------------------------------------------
    -- BLE transport framing within L2CAP CoC SDU:
    --
    --   [sdu_len: 2 bytes LE] [content: sdu_len bytes]
    --
    -- Where content is one or more concatenated FMP frames, each with:
    --
    --   [fmp_len: 2 bytes BE] [fmp_data: fmp_len bytes]
    --
    -- The L2CAP layer may or may not have consumed the SDU length prefix
    -- depending on the L2CAP mode.  We detect which format we received:
    --   1. If the first 2 bytes (as LE u16) + 2 equals the tvb length,
    --      we have SDU-prefixed data.
    --   2. If the first 2 bytes (as BE u16) + 2 <= tvb length and the
    --      resulting FMP frame parses cleanly, we have direct BLE frames.
    --   3. Otherwise, treat the entire tvb as a single FMP frame (raw
    --      transport like UDP/Ethernet).
    -----------------------------------------------------------------------

    local offset = 0
    local sdu_candidate = tvb(0, 2):le_uint()

    if sdu_candidate + 2 == buf_len and buf_len >= 4 then
        ---------------------------------------------------------------
        -- Case 1: SDU-length-prefixed (BLE L2CAP CoC)
        ---------------------------------------------------------------
        local sdu_tree = tree:add(fmp_proto, tvb(0, 2), "FMP BLE Transport")
        sdu_tree:add_le(hf_sdu_length, tvb(0, 2))

        offset = 2  -- skip SDU length

        -- Loop through concatenated [len:2 BE][data:len] frames
        while offset + 2 <= buf_len do
            local frame_len = tvb(offset, 2):uint()  -- big-endian
            if frame_len == 0 or offset + 2 + frame_len > buf_len then
                break
            end

            local frame_tree = tree:add(fmp_proto, tvb(offset, 2 + frame_len),
                string.format("FMP BLE Frame (%d bytes)", frame_len))
            frame_tree:add(hf_frame_length, tvb(offset, 2))

            local frame_tvb = tvb(offset + 2, frame_len):tvb("FMP Frame")
            dissect_fmp_frame(frame_tvb, pinfo, frame_tree)

            offset = offset + 2 + frame_len
        end

    elseif buf_len >= 4 then
        -- Check if this looks like raw [len:2 BE][data:len] frames
        -- (SDU length already consumed by L2CAP layer)
        local be_len = tvb(0, 2):uint()  -- big-endian
        if be_len > 0 and be_len + 2 <= buf_len then
            -- Peek at what would be the FMP common prefix
            local peek_offset = 2
            if peek_offset + COMMON_PREFIX_SIZE <= buf_len then
                local ver_phase_byte = tvb(peek_offset, 1):uint()
                local peek_version = bit.rshift(bit.band(ver_phase_byte, 0xF0), 4)
                local peek_phase   = bit.band(ver_phase_byte, 0x0F)

                if peek_version == 0 and peek_phase <= 2 then
                    ---------------------------------------------------
                    -- Case 2: Direct BLE framing (SDU stripped by L2CAP)
                    ---------------------------------------------------
                    while offset + 2 <= buf_len do
                        local frame_len = tvb(offset, 2):uint()
                        if frame_len == 0 or offset + 2 + frame_len > buf_len then
                            break
                        end

                        local frame_tree = tree:add(fmp_proto, tvb(offset, 2 + frame_len),
                            string.format("FMP BLE Frame (%d bytes)", frame_len))
                        frame_tree:add(hf_frame_length, tvb(offset, 2))

                        local frame_tvb = tvb(offset + 2, frame_len):tvb("FMP Frame")
                        dissect_fmp_frame(frame_tvb, pinfo, frame_tree)

                        offset = offset + 2 + frame_len
                    end
                    return
                end
            end
        end

        ---------------------------------------------------------------
        -- Case 3: Raw FMP frame (no BLE transport framing)
        -- (UDP, Ethernet, or single-frame L2CAP SDU without prefix)
        ---------------------------------------------------------------
        dissect_fmp_frame(tvb, pinfo, tree)
    else
        -- Too short for any meaningful dissection
        return 0
    end
end

---------------------------------------------------------------------------
-- Register on L2CAP PSM table (for HCI-level / regular captures)
-- AND on L2CAP CID table (for btmon monitor-mode captures where PSM is
-- not available and frames appear on the dynamic CID).
---------------------------------------------------------------------------
local l2cap_psm_table = DissectorTable.get("btl2cap.psm")
l2cap_psm_table:add(FMP_PSM, fmp_proto)

-- Monitor-mode (btmon / BlueZ monitor) demuxes by CID, not PSM.
-- L2CAP CoC dynamic CIDs are in the range 0x0040–0xFFFF.
-- We register a heuristic on the CID table as a fallback so that
-- btmonitor captures are decoded.  The CID-based dissector tries to
-- parse as FMP; if the frame doesn't look valid it returns 0 and
-- Wireshark falls through to the default L2CAP dissector.
local l2cap_cid_table = DissectorTable.get("btl2cap.cid")
if l2cap_cid_table then
    -- Register on all dynamic CIDs that might carry FMP traffic.
    -- In practice btmonitor reuses a small set; we cover the common
    -- range (0x0040–0x007F) used by the Linux L2CAP channel allocator.
    for cid = 0x0040, 0x007F do
        l2cap_cid_table:add(cid, fmp_proto)
    end
end
