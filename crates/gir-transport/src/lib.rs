pub mod client;
pub mod credentials;
pub mod pktline;
pub mod refs_discovery;
pub mod server;
pub mod sideband;

pub use client::{
    clone_repository, connect_transport, fetch_from_remote, push_to_remote, FetchOutcome,
    LocalTransport, PushOptions, PushOutcome, PushedRefUpdate, SmartHttpTransport, SshTransport,
    TransportClient, UpdatedTrackingRef,
};
pub use credentials::{query_credential_helper, Credentials, RemoteEndpoint};
pub use pktline::{encode_delim_pkt, encode_flush_pkt, encode_pkt_line, PktLine, PktLineReader, PktLineWriter};
pub use refs_discovery::{format_advertised_refs, parse_advertised_refs, AdvertisedRefs};
pub use server::{
    advertise_receive_pack_refs, advertise_upload_pack_refs, handle_receive_pack,
    handle_upload_pack,
};
pub use sideband::{demux_sideband_stream, write_sideband_frames, SidebandChannel};
