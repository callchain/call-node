//! Wire protocol: channel multiplexing helpers.

/// First byte of each message encodes the channel ID for multiplexing.
const CHANNEL_PREFIX_LEN: usize = 1;

pub(crate) fn encode_with_channel(channel: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(CHANNEL_PREFIX_LEN + payload.len());
    buf.push(channel as u8);
    buf.extend_from_slice(payload);
    buf
}

pub(crate) fn decode_with_channel(data: &[u8]) -> Option<(u64, &[u8])> {
    if data.is_empty() {
        return None;
    }
    Some((data[0] as u64, &data[1..]))
}
