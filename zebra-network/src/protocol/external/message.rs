//! Definitions of network messages.

use std::{error::Error, fmt, sync::Arc};

use chrono::{DateTime, Utc};

use zebra_chain::{
    block::{self, Block},
    transaction::UnminedTx,
};

use crate::meta_addr::MetaAddr;

use super::{addr::AddrInVersion, inv::InventoryHash, types::*};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(any(test, feature = "proptest-impl"))]
use zebra_chain::serialization::arbitrary::datetime_full;

/// A Bitcoin-like network message for the Zcash protocol.
///
/// The Zcash network protocol is mostly inherited from Bitcoin, and a list of
/// Bitcoin network messages can be found [on the Bitcoin
/// wiki][btc_wiki_protocol].
///
/// That page describes the wire format of the messages, while this enum stores
/// an internal representation. The internal representation is unlinked from the
/// wire format, and the translation between the two happens only during
/// serialization and deserialization. For instance, Bitcoin identifies messages
/// by a 12-byte ascii command string; we consider this a serialization detail
/// and use the enum discriminant instead. (As a side benefit, this also means
/// that we have a clearly-defined validation boundary for network messages
/// during serialization).
///
/// [btc_wiki_protocol]: https://en.bitcoin.it/wiki/Protocol_documentation
#[derive(Clone, Eq, PartialEq, Debug)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum Message {
    /// A `version` message.
    ///
    /// Note that although this is called `version` in Bitcoin, its role is really
    /// analogous to a `ClientHello` message in TLS, used to begin a handshake, and
    /// is distinct from a simple version number.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#version)
    Version {
        /// The network version number supported by the sender.
        version: Version,

        /// The network services advertised by the sender.
        services: PeerServices,

        /// The time when the version message was sent.
        ///
        /// This is a 64-bit field. Zebra rejects out-of-range times as invalid.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "datetime_full()")
        )]
        timestamp: DateTime<Utc>,

        /// The network address of the node receiving this message, and its
        /// advertised network services.
        ///
        /// Q: how does the handshake know the remote peer's services already?
        address_recv: AddrInVersion,

        /// The network address of the node sending this message, and its
        /// advertised network services.
        address_from: AddrInVersion,

        /// Node random nonce, randomly generated every time a version
        /// packet is sent. This nonce is used to detect connections
        /// to self.
        nonce: Nonce,

        /// The Zcash user agent advertised by the sender.
        user_agent: String,

        /// The last block received by the emitting node.
        start_height: block::Height,

        /// Whether the remote peer should announce relayed
        /// transactions or not, see [BIP 0037](https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki)
        ///
        /// Zebra does not implement the bloom filters in BIP 0037.
        /// Instead, it only relays:
        /// - newly verified best chain block hashes and mempool transaction IDs,
        /// - after it reaches the chain tip.
        relay: bool,
    },

    /// A `verack` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#verack)
    Verack,

    /// A `ping` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#ping)
    Ping(
        /// A nonce unique to this [`Ping`] message.
        Nonce,
    ),

    /// A `pong` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#pong)
    Pong(
        /// The nonce from the [`Ping`] message this was in response to.
        Nonce,
    ),

    /// A `reject` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#reject)
    Reject {
        /// Type of message rejected.
        // It's unclear if this is strictly limited to message command
        // codes, so leaving it a String.
        message: String,

        /// RejectReason code relating to rejected message.
        ccode: RejectReason,

        /// Human-readable version of rejection reason.
        reason: String,

        /// Optional extra data provided for some errors.
        // Currently, all errors which provide this field fill it with
        // the TXID or block header hash of the object being rejected,
        // so the field is 32 bytes.
        data: Option<[u8; 32]>,
    },

    /// A `getaddr` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getaddr)
    GetAddr,

    /// A sent or received `addr` message, or a received `addrv2` message.
    ///
    /// Currently, Zebra:
    /// - sends and receives `addr` messages,
    /// - parses received `addrv2` messages, ignoring some address types,
    /// - but does not send `addrv2` messages.
    ///
    ///
    /// The list contains `0..=MAX_META_ADDR` addresses.
    ///
    /// Because some address types are ignored, the deserialized vector can be empty,
    /// even if the peer sent addresses. This is not an error.
    ///
    /// [addr Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#addr)
    /// [addrv2 ZIP 155](https://zips.z.cash/zip-0155#specification)
    Addr(Vec<MetaAddr>),

    /// A `getblocks` message.
    ///
    /// `known_blocks` is a series of known block hashes spaced out along the
    /// peer's best chain. The remote peer uses them to compute the intersection
    /// of its best chain and determine the blocks following the intersection
    /// point.
    ///
    /// The peer responds with an `inv` packet with the hashes of subsequent blocks.
    /// If supplied, the `stop` parameter specifies the last header to request.
    /// Otherwise, an inv packet with the maximum number (500) are sent.
    ///
    /// The known blocks list contains zero or more block hashes.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getheaders)
    GetBlocks {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last header to request.
        stop: Option<block::Hash>,
    },

    /// An `inv` message.
    ///
    /// Allows a node to advertise its knowledge of one or more
    /// objects. It can be received unsolicited, or in reply to
    /// `getblocks`.
    ///
    /// The list contains zero or more inventory hashes.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#inv)
    /// [ZIP-239](https://zips.z.cash/zip-0239)
    Inv(Vec<InventoryHash>),

    /// A `getheaders` message.
    ///
    /// `known_blocks` is a series of known block hashes spaced out along the
    /// peer's best chain. The remote peer uses them to compute the intersection
    /// of its best chain and determine the blocks following the intersection
    /// point.
    ///
    /// The peer responds with an `headers` packet with the headers of subsequent blocks.
    /// If supplied, the `stop` parameter specifies the last header to request.
    /// Otherwise, the maximum number of block headers (160) are sent.
    ///
    /// The known blocks list contains zero or more block hashes.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getheaders)
    GetHeaders {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last header to request.
        stop: Option<block::Hash>,
    },

    /// A `headers` message.
    ///
    /// Returns block headers in response to a getheaders packet.
    ///
    /// Each block header is accompanied by a transaction count.
    ///
    /// The list contains zero or more headers.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#headers)
    Headers(Vec<block::CountedHeader>),

    /// A `getdata` message.
    ///
    /// `getdata` is used in response to `inv`, to retrieve the
    /// content of a specific object, and is usually sent after
    /// receiving an `inv` packet, after filtering known elements.
    ///
    /// `zcashd` returns requested items in a single batch of messages.
    /// Missing blocks are silently skipped. Missing transaction hashes are
    /// included in a single `NotFound` message following the transactions.
    /// Other item or non-item messages can come before or after the batch.
    ///
    /// The list contains zero or more inventory hashes.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getdata)
    /// [ZIP-239](https://zips.z.cash/zip-0239)
    /// [zcashd code](https://github.com/zcash/zcash/blob/e7b425298f6d9a54810cb7183f00be547e4d9415/src/main.cpp#L5523)
    GetData(Vec<InventoryHash>),

    /// A `block` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#block)
    Block(Arc<Block>),

    /// A `tx` message.
    ///
    /// This message is used to advertise unmined transactions for the mempool.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#tx)
    Tx(UnminedTx),

    /// A `notfound` message.
    ///
    /// When a peer requests a list of transaction hashes, `zcashd` returns:
    ///   - a batch of messages containing found transactions, then
    ///   - a `NotFound` message containing a list of transaction hashes that
    ///      aren't available in its mempool or state.
    ///
    /// But when a peer requests blocks or headers, any missing items are
    /// silently skipped, without any `NotFound` messages.
    ///
    /// The list contains zero or more inventory hashes.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#notfound)
    /// [ZIP-239](https://zips.z.cash/zip-0239)
    /// [zcashd code](https://github.com/zcash/zcash/blob/e7b425298f6d9a54810cb7183f00be547e4d9415/src/main.cpp#L5632)
    // See note above on `Inventory`.
    NotFound(Vec<InventoryHash>),

    /// A `mempool` message.
    ///
    /// This was defined in [BIP35], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#mempool)
    /// [BIP35]: https://github.com/bitcoin/bips/blob/master/bip-0035.mediawiki
    Mempool,

    /// A `filterload` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// Zebra currently ignores this message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    FilterLoad {
        /// The filter itself is simply a bit field of arbitrary
        /// byte-aligned size. The maximum size is 36,000 bytes.
        filter: Filter,

        /// The number of hash functions to use in this filter. The
        /// maximum value allowed in this field is 50.
        hash_functions_count: u32,

        /// A random value to add to the seed value in the hash
        /// function used by the bloom filter.
        tweak: Tweak,

        /// A set of flags that control how matched items are added to the filter.
        flags: u8,
    },

    /// A `filteradd` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// Zebra currently ignores this message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    FilterAdd {
        /// The data element to add to the current filter.
        // The data field must be smaller than or equal to 520 bytes
        // in size (the maximum size of any potentially matched
        // object).
        //
        // A Vec instead of [u8; 520] because of needed traits.
        data: Vec<u8>,
    },

    /// A `filterclear` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// Zebra currently ignores this message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    FilterClear,
}

impl<E> From<E> for Message
where
    E: Error,
{
    fn from(e: E) -> Self {
        Message::Reject {
            message: e.to_string(),

            // The generic case, impls for specific error types should
            // use specific varieties of `RejectReason`.
            ccode: RejectReason::Other,

            reason: e.source().unwrap().to_string(),

            // Allow this to be overridden but not populated by default, methinks.
            data: None,
        }
    }
}

/// Reject Reason CCodes
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#reject)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[repr(u8)]
#[allow(missing_docs)]
pub enum RejectReason {
    Malformed = 0x01,
    Invalid = 0x10,
    Obsolete = 0x11,
    Duplicate = 0x12,
    Nonstandard = 0x40,
    Dust = 0x41,
    InsufficientFee = 0x42,
    Checkpoint = 0x43,
    Other = 0x50,
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&match self {
            Message::Version {
                version,
                address_recv,
                address_from,
                user_agent,
                ..
            } => format!(
                "version {{ network: {}, recv: {},_from: {}, user_agent: {:?} }}",
                version,
                address_recv.addr(),
                address_from.addr(),
                user_agent,
            ),
            Message::Verack => "verack".to_string(),
            Message::Ping(_) => "ping".to_string(),
            Message::Pong(_) => "pong".to_string(),
            Message::Reject {
                message,
                reason,
                data,
                ..
            } => format!(
                "reject {{ message: {:?}, reason: {:?}, data: {} }}",
                message,
                reason,
                if data.is_some() { "Some" } else { "None" },
            ),
            Message::GetAddr => "getaddr".to_string(),
            Message::Addr(addrs) => format!("addr {{ addrs: {} }}", addrs.len()),
            Message::GetBlocks { known_blocks, stop } => format!(
                "getblocks {{ known_blocks: {}, stop: {} }}",
                known_blocks.len(),
                if stop.is_some() { "Some" } else { "None" },
            ),
            Message::Inv(invs) => format!("inv {{ invs: {} }}", invs.len()),
            Message::GetHeaders { known_blocks, stop } => format!(
                "getheaders {{ known_blocks: {}, stop: {} }}",
                known_blocks.len(),
                if stop.is_some() { "Some" } else { "None" },
            ),
            Message::Headers(headers) => format!("headers {{ headers: {} }}", headers.len()),
            Message::GetData(invs) => format!("getdata {{ invs: {} }}", invs.len()),
            Message::Block(_) => "block".to_string(),
            Message::Tx(_) => "tx".to_string(),
            Message::NotFound(invs) => format!("notfound {{ invs: {} }}", invs.len()),
            Message::Mempool => "mempool".to_string(),
            Message::FilterLoad { .. } => "filterload".to_string(),
            Message::FilterAdd { .. } => "filteradd".to_string(),
            Message::FilterClear => "filterclear".to_string(),
        })
    }
}
