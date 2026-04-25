--[[======================================================================
  FIPS Protocol Dissector for Wireshark
  Mesh Protocol (FMP) + Session Protocol (FSP) over BLE L2CAP CoC

  Wire format reference: docs/design/fips-wire-formats.md

  INSTALLATION
  ------------
  1. Copy this file to ~/.wireshark/plugins/ (or ~/.local/lib/wireshark/plugins/
     on some systems). Create the directory if it doesn't exist.
  2. Alternatively: Edit -> Preferences -> Protocols -> FIPS -> set the path.
  3. Restart Wireshark or reload Lua plugins: Analyze -> Reload Lua Plugins.
  4. Capture BLE traffic with btmon or Wireshark's built-in Bluetooth
     capture. The dissector auto-registers on L2CAP PSM 0x0085.

  For btmon captures saved as pcap:
    btmon -w capture.pcap
    wireshark capture.pcap

  For live BLE capture in Wireshark, the Bluetooth L2CAP layer must be
  present. Filter with: fips or btl2cap.psm == 0x0085

  OVERVIEW
  --------
  All FMP/FSP packets share a 4-byte common prefix:
    Byte 0: (version:4 | phase:4)  — version in high nibble, phase in low
    Byte 1: flags
    Bytes 2-3: payload_len (u16 LE)

  FMP (link layer, on the wire over L2CAP):
    Phase 0x0 = Established (16-byte outer header = AAD, encrypted inner)
    Phase 0x1 = Noise IK msg1 (handshake initiation)
    Phase 0x2 = Noise IK msg2 (handshake response)

  FSP (session layer, carried inside encrypted FMP frames):
    Phase 0x0 = Established (12-byte header = AAD, encrypted inner)
    Phase 0x1 = SessionSetup (Noise XK msg1)
    Phase 0x2 = SessionAck (Noise XK msg2)
    Phase 0x3 = SessionMsg3 (Noise XK msg3)

  This dissector parses FMP packets on the wire. FSP sub-dissection is
  attempted for plaintext FSP error signals (U flag) inside SessionDatagram
  when decrypted, and for direct FSP captures.
======================================================================]]

------------------------------------------------------------------------
-- FMP Protocol
------------------------------------------------------------------------
local fmp_proto = Proto("fips.fmp", "FIPS Mesh Protocol (FMP)")

-- Common prefix fields
local f_version_phase = ProtoField.uint8("fips.version_phase", "Version / Phase", base.HEX)
local f_version       = ProtoField.uint8("fips.version", "Version", base.DEC, nil, 0xF0)
local f_phase         = ProtoField.uint8("fips.phase", "Phase", base.HEX, nil, 0x0F)
local f_flags         = ProtoField.uint8("fips.flags", "Flags", base.HEX)
local f_payload_len   = ProtoField.uint16("fips.payload_len", "Payload Length", base.DEC)

-- FMP flag bits (established phase)
local f_flag_k  = ProtoField.bool("fips.flags.key_epoch", "K (Key Epoch)", base.NONE, nil, 0x01)
local f_flag_ce = ProtoField.bool("fips.flags.congestion", "CE (Congestion Experienced)", base.NONE, nil, 0x02)
local f_flag_sp = ProtoField.bool("fips.flags.spin", "SP (Spin Bit)", base.NONE, nil, 0x04)

-- Established frame fields
local f_receiver_idx  = ProtoField.uint32("fips.receiver_idx", "Receiver Index", base.HEX)
local f_counter       = ProtoField.uint64("fips.counter", "Counter", base.DEC)
local f_aad           = ProtoField.bytes("fips.aad", "AAD (16-byte outer header)")
local f_ciphertext    = ProtoField.bytes("fips.ciphertext", "Encrypted Payload + AEAD Tag")

-- Handshake msg1 fields
local f_sender_idx    = ProtoField.uint32("fips.sender_idx", "Sender Index", base.HEX)
local f_ephemeral_pub = ProtoField.bytes("fips.ephemeral_pubkey", "Ephemeral Public Key")
local f_enc_static    = ProtoField.bytes("fips.encrypted_static", "Encrypted Static Key + Tag")
local f_enc_epoch     = ProtoField.bytes("fips.encrypted_epoch", "Encrypted Epoch + Tag")

-- Inner header (visible only if decrypted)
local f_inner_ts      = ProtoField.uint32("fips.inner.timestamp", "Inner Timestamp", base.DEC)
local f_inner_msgtype = ProtoField.uint8("fips.inner.msg_type", "Inner Message Type", base.HEX)

-- Noise payload container
local f_noise_payload = ProtoField.bytes("fips.noise_payload", "Noise Payload")

-- FMP message type names
local FMP_MSG_TYPES = {
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

-- FMP phase names
local FMP_PHASES = {
    [0x0] = "Established",
    [0x1] = "Noise IK msg1",
    [0x2] = "Noise IK msg2",
}

-- Disconnect reason codes
local DISCONNECT_REASONS = {
    [0x00] = "Shutdown",
    [0x01] = "Restart",
    [0x02] = "ProtocolError",
    [0x03] = "TransportFailure",
    [0x04] = "ResourceExhaustion",
    [0x05] = "SecurityViolation",
    [0x06] = "ConfigurationChange",
    [0x07] = "Timeout",
    [0xFF] = "Other",
}

fmp_proto.fields = {
    f_version_phase, f_version, f_phase, f_flags, f_payload_len,
    f_flag_k, f_flag_ce, f_flag_sp,
    f_receiver_idx, f_counter, f_aad, f_ciphertext,
    f_sender_idx,
    f_ephemeral_pub, f_enc_static, f_enc_epoch,
    f_inner_ts, f_inner_msgtype,
    f_noise_payload,
}

------------------------------------------------------------------------
-- FSP Protocol
------------------------------------------------------------------------
local fsp_proto = Proto("fips.fsp", "FIPS Session Protocol (FSP)")

-- FSP-specific fields
local f_fsp_counter        = ProtoField.uint64("fips.fsp.counter", "Counter", base.DEC)
local f_fsp_aad            = ProtoField.bytes("fips.fsp.aad", "AAD (12-byte header)")
local f_fsp_ciphertext     = ProtoField.bytes("fips.fsp.ciphertext", "Encrypted Payload + AEAD Tag")
local f_fsp_inner_ts       = ProtoField.uint32("fips.fsp.inner.timestamp", "Inner Timestamp", base.DEC)
local f_fsp_inner_msgtype  = ProtoField.uint8("fips.fsp.inner.msg_type", "Inner Message Type", base.HEX)
local f_fsp_inner_flags    = ProtoField.uint8("fips.fsp.inner.flags", "Inner Flags", base.HEX)
local f_fsp_flag_cp        = ProtoField.bool("fips.fsp.flags.coords_present", "CP (Coords Present)", base.NONE, nil, 0x01)
local f_fsp_flag_k         = ProtoField.bool("fips.fsp.flags.key_epoch", "K (Key Epoch)", base.NONE, nil, 0x02)
local f_fsp_flag_u         = ProtoField.bool("fips.fsp.flags.unencrypted", "U (Unencrypted)", base.NONE, nil, 0x04)

-- FSP handshake fields
local f_fsp_hs_flags        = ProtoField.uint8("fips.fsp.hs.flags", "Handshake Flags", base.HEX)
local f_fsp_src_coords_cnt  = ProtoField.uint16("fips.fsp.src_coords_count", "Source Coords Count", base.DEC)
local f_fsp_src_coords      = ProtoField.bytes("fips.fsp.src_coords", "Source Coordinates")
local f_fsp_dst_coords_cnt  = ProtoField.uint16("fips.fsp.dst_coords_count", "Dest Coords Count", base.DEC)
local f_fsp_dst_coords      = ProtoField.bytes("fips.fsp.dst_coords", "Dest Coordinates")
local f_fsp_hs_len          = ProtoField.uint16("fips.fsp.handshake_len", "Handshake Payload Length", base.DEC)
local f_fsp_hs_payload      = ProtoField.bytes("fips.fsp.handshake_payload", "Noise Handshake Payload")

-- FSP established cleartext coords
local f_fsp_clr_src_cnt     = ProtoField.uint16("fips.fsp.clr.src_coords_count", "Cleartext Src Coords Count", base.DEC)
local f_fsp_clr_src_coords  = ProtoField.bytes("fips.fsp.clr.src_coords", "Cleartext Source Coordinates")
local f_fsp_clr_dst_cnt     = ProtoField.uint16("fips.fsp.clr.dst_coords_count", "Cleartext Dst Coords Count", base.DEC)
local f_fsp_clr_dst_coords  = ProtoField.bytes("fips.fsp.clr.dst_coords", "Cleartext Dest Coordinates")

-- FSP plaintext error signal fields
local f_fsp_err_msgtype     = ProtoField.uint8("fips.fsp.err.msg_type", "Error Message Type", base.HEX)
local f_fsp_err_flags       = ProtoField.uint8("fips.fsp.err.flags", "Error Flags", base.HEX)
local f_fsp_err_dest_addr   = ProtoField.bytes("fips.fsp.err.dest_addr", "Destination NodeAddr")
local f_fsp_err_reporter    = ProtoField.bytes("fips.fsp.err.reporter", "Reporter NodeAddr")
local f_fsp_err_mtu         = ProtoField.uint16("fips.fsp.err.mtu", "Bottleneck MTU", base.DEC)
local f_fsp_err_coords_cnt  = ProtoField.uint16("fips.fsp.err.coords_count", "Stale Coords Count", base.DEC)
local f_fsp_err_coords      = ProtoField.bytes("fips.fsp.err.coords", "Stale Coordinates")

-- FSP message type names
local FSP_MSG_TYPES = {
    [0x10] = "Data",
    [0x11] = "SenderReport",
    [0x12] = "ReceiverReport",
    [0x13] = "PathMtuNotification",
    [0x14] = "CoordsWarmup",
    [0x20] = "CoordsRequired",
    [0x21] = "PathBroken",
    [0x22] = "MtuExceeded",
}

local FSP_PHASES = {
    [0x0] = "Established",
    [0x1] = "SessionSetup",
    [0x2] = "SessionAck",
    [0x3] = "SessionMsg3",
}

fsp_proto.fields = {
    f_fsp_counter, f_fsp_aad, f_fsp_ciphertext,
    f_fsp_inner_ts, f_fsp_inner_msgtype, f_fsp_inner_flags,
    f_fsp_flag_cp, f_fsp_flag_k, f_fsp_flag_u,
    f_fsp_hs_flags,
    f_fsp_src_coords_cnt, f_fsp_src_coords,
    f_fsp_dst_coords_cnt, f_fsp_dst_coords,
    f_fsp_hs_len, f_fsp_hs_payload,
    f_fsp_clr_src_cnt, f_fsp_clr_src_coords,
    f_fsp_clr_dst_cnt, f_fsp_clr_dst_coords,
    f_fsp_err_msgtype, f_fsp_err_flags,
    f_fsp_err_dest_addr, f_fsp_err_reporter,
    f_fsp_err_mtu, f_fsp_err_coords_cnt, f_fsp_err_coords,
}

------------------------------------------------------------------------
-- Helper: safe buffer slice extraction (never crash on short packets)
------------------------------------------------------------------------
local function safe_uint8(buf, offset)
    if offset + 1 > buf:len() then return nil end
    return buf(offset, 1):uint()
end

local function safe_uint16(buf, offset)
    if offset + 2 > buf:len() then return nil end
    return buf(offset, 2):uint()
end

local function safe_uint32(buf, offset)
    if offset + 4 > buf:len() then return nil end
    return buf(offset, 4):uint()
end

local function safe_uint64(buf, offset)
    if offset + 8 > buf:len() then return nil end
    return buf(offset, 8):uint64()
end

local function safe_bytes(buf, offset, len)
    if offset + len > buf:len() then return nil end
    return buf(offset, len)
end

------------------------------------------------------------------------
-- Helper: read common prefix (shared by FMP and FSP)
-- Returns version, phase, flags, payload_len, offset_after_prefix
------------------------------------------------------------------------
local function read_common_prefix(buf)
    if buf:len() < 4 then
        return nil -- too short
    end
    local vp = buf(0, 1):uint()
    local version = bit.rshift(bit.band(vp, 0xF0), 4)
    local phase   = bit.band(vp, 0x0F)
    local flags   = buf(1, 1):uint()
    local payload_len = buf(2, 2):uint() -- u16 LE (Wireshark TVB handles endianness)
    return version, phase, flags, payload_len
end

------------------------------------------------------------------------
-- Helper: add common prefix to tree
------------------------------------------------------------------------
local function add_common_prefix(tree, buf, proto_fields)
    tree:add(proto_fields.version_phase, buf(0, 1))
    tree:add(proto_fields.version, buf(0, 1))
    tree:add(proto_fields.phase, buf(0, 1))
    tree:add(proto_fields.flags, buf(1, 1))
    tree:add(proto_fields.payload_len, buf(2, 2))
end

------------------------------------------------------------------------
-- Helper: add FMP flag bit fields
------------------------------------------------------------------------
local function add_fmp_flag_bits(tree, buf)
    tree:add(f_flag_k,  buf(1, 1))
    tree:add(f_flag_ce, buf(1, 1))
    tree:add(f_flag_sp, buf(1, 1))
end

------------------------------------------------------------------------
-- Helper: add FSP flag bit fields
------------------------------------------------------------------------
local function add_fsp_flag_bits(tree, buf)
    tree:add(f_fsp_flag_cp, buf(1, 1))
    tree:add(f_fsp_flag_k,  buf(1, 1))
    tree:add(f_fsp_flag_u,  buf(1, 1))
end

------------------------------------------------------------------------
-- Dissect FMP Established frame (phase 0x0)
------------------------------------------------------------------------
local function dissect_fmp_established(buf, pinfo, tree)
    -- Outer header is 16 bytes: 4 prefix + 4 receiver_idx + 8 counter
    if buf:len() < 16 then
        tree:add_expert_info(PI_MALFORMED, PI_ERROR, "FMP Established frame too short (need 16-byte outer header)")
        return
    end

    local flags = buf(1, 1):uint()
    local payload_len = buf(2, 2):uint()

    -- Outer header subtree
    local outer_tree = tree:add(fmp_proto, buf(0, 16), "Outer Header (AAD)")
    add_common_prefix(outer_tree, buf, {
        version_phase = f_version_phase,
        version       = f_version,
        phase         = f_phase,
        flags         = f_flags,
        payload_len   = f_payload_len,
    })

    -- Flag bit subtree
    local flags_tree = outer_tree:add(f_flags, buf(1, 1), "Flags")
    add_fmp_flag_bits(flags_tree, buf)

    outer_tree:add(f_receiver_idx, buf(4, 4))
    local counter_val = buf(8, 8):uint64()
    outer_tree:add(f_counter, buf(8, 8))

    -- AAD reference (entire 16-byte outer header)
    tree:add(f_aad, buf(0, 16))

    -- Remaining bytes are ciphertext + AEAD tag
    local remaining = buf:len() - 16
    if remaining > 0 then
        tree:add(f_ciphertext, buf(16, remaining))
    end

    -- Info column
    local receiver_idx = buf(4, 4):uint()
    pinfo.cols.info = string.format("Established rx_idx=0x%08x counter=%s len=%d",
        receiver_idx, tostring(counter_val), payload_len)
end

------------------------------------------------------------------------
-- Dissect FMP Noise IK msg1 (phase 0x1)
------------------------------------------------------------------------
local function dissect_fmp_msg1(buf, pinfo, tree)
    -- Minimum: 4 prefix + 4 sender_idx + 33 ephemeral = 41 bytes
    if buf:len() < 8 then
        tree:add_expert_info(PI_MALFORMED, PI_ERROR, "FMP msg1 too short")
        return
    end

    local prefix_tree = tree:add(fmp_proto, buf(0, 4), "Common Prefix")
    add_common_prefix(prefix_tree, buf, {
        version_phase = f_version_phase,
        version       = f_version,
        phase         = f_phase,
        flags         = f_flags,
        payload_len   = f_payload_len,
    })

    tree:add(f_sender_idx, buf(4, 4))

    local sender_idx = buf(4, 4):uint()

    -- Noise msg1 breakdown (offset 8)
    local noise_tree = tree:add(fmp_proto, buf(8), "Noise IK msg1 (106 bytes)")

    if buf:len() >= 8 + 33 then
        noise_tree:add(f_ephemeral_pub, buf(8, 33))
            :append_text(" (compressed secp256k1)")
    end

    if buf:len() >= 8 + 33 + 49 then
        noise_tree:add(f_enc_static, buf(41, 49))
            :append_text(" (33-byte static + 16-byte tag)")
    end

    if buf:len() >= 8 + 33 + 49 + 24 then
        noise_tree:add(f_enc_epoch, buf(90, 24))
            :append_text(" (8-byte epoch + 16-byte tag)")
    end

    pinfo.cols.info = string.format("IK msg1 sender_idx=0x%08x", sender_idx)
end

------------------------------------------------------------------------
-- Dissect FMP Noise IK msg2 (phase 0x2)
------------------------------------------------------------------------
local function dissect_fmp_msg2(buf, pinfo, tree)
    -- Minimum: 4 prefix + 4 sender_idx + 4 receiver_idx + 33 ephemeral = 45 bytes
    if buf:len() < 12 then
        tree:add_expert_info(PI_MALFORMED, PI_ERROR, "FMP msg2 too short")
        return
    end

    local prefix_tree = tree:add(fmp_proto, buf(0, 4), "Common Prefix")
    add_common_prefix(prefix_tree, buf, {
        version_phase = f_version_phase,
        version       = f_version,
        phase         = f_phase,
        flags         = f_flags,
        payload_len   = f_payload_len,
    })

    tree:add(f_sender_idx, buf(4, 4))
    tree:add(f_receiver_idx, buf(8, 4))

    local sender_idx = buf(4, 4):uint()
    local receiver_idx = buf(8, 4):uint()

    -- Noise msg2 breakdown (offset 12)
    local noise_tree = tree:add(fmp_proto, buf(12), "Noise IK msg2 (57 bytes)")

    if buf:len() >= 12 + 33 then
        noise_tree:add(f_ephemeral_pub, buf(12, 33))
            :append_text(" (compressed secp256k1)")
    end

    if buf:len() >= 12 + 33 + 24 then
        noise_tree:add(f_enc_epoch, buf(45, 24))
            :append_text(" (8-byte epoch + 16-byte tag)")
    end

    pinfo.cols.info = string.format("IK msg2 sender_idx=0x%08x rx_idx=0x%08x",
        sender_idx, receiver_idx)
end

------------------------------------------------------------------------
-- FMP dissector entry point
------------------------------------------------------------------------
function fmp_proto.dissector(buf, pinfo, tree)
    local buf_len = buf:len()
    if buf_len < 4 then return 0 end -- not enough for common prefix

    local version, phase, flags, payload_len = read_common_prefix(buf)
    if not version then return 0 end

    local subtree = tree:add(fmp_proto, buf(), "FIPS Mesh Protocol")
    pinfo.cols.protocol = "FIPS"

    -- Validate version
    if version ~= 0 then
        subtree:add_expert_info(PI_PROTOCOL, PI_WARN,
            string.format("Unknown FIPS version: %d", version))
    end

    local phase_name = FMP_PHASES[phase] or string.format("Unknown(0x%x)", phase)
    subtree:append_text(string.format(" — %s", phase_name))

    -- Dispatch by phase
    if phase == 0x0 then
        dissect_fmp_established(buf, pinfo, subtree)
    elseif phase == 0x1 then
        dissect_fmp_msg1(buf, pinfo, subtree)
    elseif phase == 0x2 then
        dissect_fmp_msg2(buf, pinfo, subtree)
    else
        -- Unknown phase — show what we can
        local prefix_tree = subtree:add(fmp_proto, buf(0, 4), "Common Prefix")
        add_common_prefix(prefix_tree, buf, {
            version_phase = f_version_phase,
            version       = f_version,
            phase         = f_phase,
            flags         = f_flags,
            payload_len   = f_payload_len,
        })
        if buf_len > 4 then
            subtree:add(f_noise_payload, buf(4))
                :append_text(string.format(" (%d bytes)", buf_len - 4))
        end
        pinfo.cols.info = string.format("Unknown phase 0x%x len=%d", phase, buf_len)
    end

    return buf_len
end

------------------------------------------------------------------------
-- FSP helper: parse variable-length coordinates block
-- Reads count (u16 LE) + (count * 16) bytes of NodeAddr entries
-- Returns bytes consumed, or nil on error
------------------------------------------------------------------------
local function parse_coords_block(buf, offset, tree, cnt_field, coords_field, label)
    local cnt_val = safe_uint16(buf, offset)
    if not cnt_val then return nil end
    tree:add(cnt_field, buf(offset, 2))
    local coords_size = cnt_val * 16
    if offset + 2 + coords_size > buf:len() then return nil end
    if coords_size > 0 then
        tree:add(coords_field, buf(offset + 2, coords_size))
            :append_text(string.format(" (%d x 16-byte NodeAddr)", cnt_val))
    end
    return 2 + coords_size
end

------------------------------------------------------------------------
-- Dissect FSP Established frame (phase 0x0)
------------------------------------------------------------------------
local function dissect_fsp_established(buf, pinfo, tree)
    -- Cleartext header is 12 bytes: 4 prefix + 8 counter
    if buf:len() < 12 then
        tree:add_expert_info(PI_MALFORMED, PI_ERROR, "FSP Established frame too short (need 12-byte header)")
        return
    end

    local flags = buf(1, 1):uint()
    local is_unencrypted = bit.band(flags, 0x04) ~= 0
    local has_coords     = bit.band(flags, 0x01) ~= 0

    -- Cleartext header subtree (AAD)
    local hdr_tree = tree:add(fsp_proto, buf(0, 12), "Cleartext Header (AAD)")
    add_common_prefix(hdr_tree, buf, {
        version_phase = f_version_phase,
        version       = f_version,
        phase         = f_phase,
        flags         = f_flags,
        payload_len   = f_payload_len,
    })

    local flags_tree = hdr_tree:add(f_flags, buf(1, 1), "Flags")
    add_fsp_flag_bits(flags_tree, buf)

    hdr_tree:add(f_fsp_counter, buf(4, 8))
    tree:add(f_fsp_aad, buf(0, 12))

    local counter_val = buf(4, 8):uint64()
    local offset = 12

    -- Optional cleartext coordinates (CP flag)
    if has_coords then
        local coords_tree = tree:add(fsp_proto, buf(offset), "Cleartext Coordinates")
        local src_len = parse_coords_block(buf, offset, coords_tree,
            f_fsp_clr_src_cnt, f_fsp_clr_src_coords, "Source")
        if not src_len then
            coords_tree:add_expert_info(PI_MALFORMED, PI_ERROR, "Truncated source coordinates")
            pinfo.cols.info = string.format("FSP Established (truncated coords)")
            return
        end
        offset = offset + src_len

        local dst_len = parse_coords_block(buf, offset, coords_tree,
            f_fsp_clr_dst_cnt, f_fsp_clr_dst_coords, "Dest")
        if not dst_len then
            coords_tree:add_expert_info(PI_MALFORMED, PI_ERROR, "Truncated dest coordinates")
            pinfo.cols.info = string.format("FSP Established (truncated coords)")
            return
        end
        offset = offset + dst_len
    end

    -- Remaining bytes
    local remaining = buf:len() - offset

    if is_unencrypted then
        -- Plaintext error signal (U flag set)
        if remaining < 2 then
            tree:add_expert_info(PI_MALFORMED, PI_ERROR, "Truncated FSP plaintext error")
            return
        end
        local err_msg_type = buf(offset, 1):uint()
        local err_msg_name = FSP_MSG_TYPES[err_msg_type] or string.format("Unknown(0x%02x)", err_msg_type)

        local err_tree = tree:add(fsp_proto, buf(offset), string.format("Plaintext Error: %s", err_msg_name))
        err_tree:add(f_fsp_err_msgtype, buf(offset, 1))
        err_tree:add(f_fsp_err_flags, buf(offset + 1, 1))

        if err_msg_type == 0x20 then -- CoordsRequired (34 bytes payload)
            if remaining >= 18 then
                err_tree:add(f_fsp_err_dest_addr, buf(offset + 2, 16))
            end
            if remaining >= 34 then
                err_tree:add(f_fsp_err_reporter, buf(offset + 18, 16))
            end
            pinfo.cols.info = string.format("FSP CoordsRequired")

        elseif err_msg_type == 0x21 then -- PathBroken (variable)
            if remaining >= 18 then
                err_tree:add(f_fsp_err_dest_addr, buf(offset + 2, 16))
            end
            if remaining >= 34 then
                err_tree:add(f_fsp_err_reporter, buf(offset + 18, 16))
            end
            local stale_cnt = safe_uint16(buf, offset + 34)
            if stale_cnt then
                err_tree:add(f_fsp_err_coords_cnt, buf(offset + 34, 2))
                local stale_bytes = stale_cnt * 16
                if offset + 36 + stale_bytes <= buf:len() then
                    err_tree:add(f_fsp_err_coords, buf(offset + 36, stale_bytes))
                end
            end
            pinfo.cols.info = string.format("FSP PathBroken")

        elseif err_msg_type == 0x22 then -- MtuExceeded (36 bytes payload)
            if remaining >= 18 then
                err_tree:add(f_fsp_err_dest_addr, buf(offset + 2, 16))
            end
            if remaining >= 34 then
                err_tree:add(f_fsp_err_reporter, buf(offset + 18, 16))
            end
            if remaining >= 36 then
                err_tree:add(f_fsp_err_mtu, buf(offset + 34, 2))
            end
            pinfo.cols.info = string.format("FSP MtuExceeded mtu=%d",
                remaining >= 36 and buf(offset + 34, 2):uint() or 0)

        else
            pinfo.cols.info = string.format("FSP Plaintext %s", err_msg_name)
        end
    else
        -- Encrypted payload + AEAD tag
        if remaining > 0 then
            tree:add(f_fsp_ciphertext, buf(offset, remaining))
                :append_text(string.format(" (%d bytes, includes 16-byte Poly1305 tag)", remaining))
        end
        pinfo.cols.info = string.format("FSP Established counter=%s len=%d%s",
            tostring(counter_val), remaining,
            has_coords and " [CP]" or "")
    end
end

------------------------------------------------------------------------
-- Dissect FSP handshake body (shared by SessionSetup, SessionAck, SessionMsg3)
------------------------------------------------------------------------
local function dissect_fsp_handshake_body(buf, pinfo, tree, offset, phase_name)
    local body_len = buf:len() - offset
    if body_len < 1 then
        tree:add_expert_info(PI_MALFORMED, PI_ERROR, "Truncated FSP handshake body")
        return
    end

    tree:add(f_fsp_hs_flags, buf(offset, 1))
    offset = offset + 1

    -- SessionMsg3 (phase 0x3) has no coordinates
    if phase_name ~= "SessionMsg3" then
        -- Source coordinates
        local src_len = parse_coords_block(buf, offset, tree,
            f_fsp_src_coords_cnt, f_fsp_src_coords, "Source")
        if not src_len then
            tree:add_expert_info(PI_MALFORMED, PI_ERROR, "Truncated source coordinates")
            return
        end
        offset = offset + src_len

        -- Destination coordinates
        local dst_len = parse_coords_block(buf, offset, tree,
            f_fsp_dst_coords_cnt, f_fsp_dst_coords, "Dest")
        if not dst_len then
            tree:add_expert_info(PI_MALFORMED, PI_ERROR, "Truncated dest coordinates")
            return
        end
        offset = offset + dst_len
    end

    -- Handshake payload
    local hs_len = safe_uint16(buf, offset)
    if not hs_len then
        tree:add_expert_info(PI_MALFORMED, PI_ERROR, "Truncated handshake length")
        return
    end
    tree:add(f_fsp_hs_len, buf(offset, 2))
    offset = offset + 2

    if offset + hs_len <= buf:len() then
        local hs_payload_tree = tree:add(f_fsp_hs_payload, buf(offset, hs_len))

        -- Describe Noise XK payload based on phase
        if phase_name == "SessionSetup" and hs_len >= 33 then
            hs_payload_tree:append_text(" (XK msg1: 33-byte ephemeral)")
        elseif phase_name == "SessionAck" and hs_len >= 57 then
            hs_payload_tree:append_text(" (XK msg2: 33-byte ephemeral + 24-byte encrypted epoch)")
        elseif phase_name == "SessionMsg3" and hs_len >= 73 then
            hs_payload_tree:append_text(" (XK msg3: 49-byte encrypted static + 24-byte encrypted epoch)")
        end
    else
        tree:add_expert_info(PI_MALFORMED, PI_ERROR,
            string.format("Handshake payload truncated (need %d, have %d)", hs_len, buf:len() - offset))
    end

    pinfo.cols.info = string.format("FSP %s", phase_name)
end

------------------------------------------------------------------------
-- Dissect FSP handshake phases (1, 2, 3)
------------------------------------------------------------------------
local function dissect_fsp_handshake(buf, pinfo, tree, phase)
    local phase_name = FSP_PHASES[phase] or string.format("Unknown(0x%x)", phase)

    local prefix_tree = tree:add(fsp_proto, buf(0, 4), "Common Prefix")
    add_common_prefix(prefix_tree, buf, {
        version_phase = f_version_phase,
        version       = f_version,
        phase         = f_phase,
        flags         = f_flags,
        payload_len   = f_payload_len,
    })

    -- Body starts at offset 4
    local body_tree = tree:add(fsp_proto, buf(4), string.format("%s Body", phase_name))
    dissect_fsp_handshake_body(buf, pinfo, body_tree, 4, phase_name)
end

------------------------------------------------------------------------
-- FSP dissector entry point
------------------------------------------------------------------------
function fsp_proto.dissector(buf, pinfo, tree)
    local buf_len = buf:len()
    if buf_len < 4 then return 0 end

    local version, phase, flags, payload_len = read_common_prefix(buf)
    if not version then return 0 end

    local subtree = tree:add(fsp_proto, buf(), "FIPS Session Protocol")
    pinfo.cols.protocol = "FIPS-FSP"

    if version ~= 0 then
        subtree:add_expert_info(PI_PROTOCOL, PI_WARN,
            string.format("Unknown FSP version: %d", version))
    end

    local phase_name = FSP_PHASES[phase] or string.format("Unknown(0x%x)", phase)
    subtree:append_text(string.format(" — %s", phase_name))

    if phase == 0x0 then
        dissect_fsp_established(buf, pinfo, subtree)
    elseif phase >= 0x1 and phase <= 0x3 then
        dissect_fsp_handshake(buf, pinfo, subtree, phase)
    else
        -- Unknown phase — show common prefix + raw payload
        local prefix_tree = subtree:add(fsp_proto, buf(0, 4), "Common Prefix")
        add_common_prefix(prefix_tree, buf, {
            version_phase = f_version_phase,
            version       = f_version,
            phase         = f_phase,
            flags         = f_flags,
            payload_len   = f_payload_len,
        })
        if buf_len > 4 then
            subtree:add(f_noise_payload, buf(4))
                :append_text(string.format(" (%d bytes)", buf_len - 4))
        end
        pinfo.cols.info = string.format("FSP Unknown phase 0x%x len=%d", phase, buf_len)
    end

    return buf_len
end

------------------------------------------------------------------------
-- Registration
------------------------------------------------------------------------

-- Register FMP on L2CAP PSM table (primary registration for BLE captures)
local ok, l2cap_psm_table = pcall(DissectorTable.get, "btl2cap.psm")
if ok and l2cap_psm_table then
    l2cap_psm_table:add(0x0085, fmp_proto)
else
    -- Fall back: try bluetooth PSM table variant
    local ok2, bt_table = pcall(DissectorTable.get, "bluetooth.psm")
    if ok2 and bt_table then
        bt_table:add(0x0085, fmp_proto)
    end
end

-- Register FSP as a heuristic dissector for UDP/TCP (useful for non-BLE captures)
-- Disabled by default; enable in Wireshark preferences if needed.
fmp_proto:register_heuristic("udp", function(buf, pinfo, tree)
    if buf:len() < 4 then return false end
    local version, phase, flags, payload_len = read_common_prefix(buf)
    if not version then return false end
    -- Heuristic: version 0, valid phase, flags zero for handshake
    if version ~= 0 then return false end
    if phase > 0x2 then return false end
    if phase == 0x0 then
        -- Established frame: need at least 16 bytes
        if buf:len() < 16 then return false end
    end
    -- Weak heuristic — only enable manually for non-BLE captures
    return false
end)

-- Preferences
fmp_proto.prefs.udp_port = Pref.uint("FIPS UDP port", 0,
    "UDP port for FIPS capture (0 = heuristic only)")
fmp_proto.prefs.tcp_port = Pref.uint("FIPS TCP port", 0,
    "TCP port for FIPS capture (0 = heuristic only)")

local fmp_udp_port = 0
local fmp_tcp_port = 0

function fmp_proto.prefs_changed(prefs)
    local udp_dissector_table = DissectorTable.get("udp.port")
    local tcp_dissector_table = DissectorTable.get("tcp.port")

    if fmp_udp_port ~= 0 then
        udp_dissector_table:remove(fmp_udp_port, fmp_proto)
    end
    if fmp_tcp_port ~= 0 then
        tcp_dissector_table:remove(fmp_tcp_port, fmp_proto)
    end

    fmp_udp_port = fmp_proto.prefs.udp_port
    fmp_tcp_port = fmp_proto.prefs.tcp_port

    if fmp_udp_port ~= 0 then
        udp_dissector_table:add(fmp_udp_port, fmp_proto)
    end
    if fmp_tcp_port ~= 0 then
        tcp_dissector_table:add(fmp_tcp_port, fmp_proto)
    end
end
