//! Field numbers from SteamDatabase/Protobufs d35adb23d7d48ad9673261f9572b60d1bfe06c12.
//! Download response URLs/headers are credentials: never serialize or log them.

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AppRequest {
    #[prost(uint32, tag = "1")]
    pub app_id: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ListingRequest {
    #[prost(uint32, tag = "1")]
    pub app_id: u32,
    #[prost(uint64, optional, tag = "2")]
    pub synced_change_number: Option<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ListingResponse {
    #[prost(uint64, optional, tag = "1")]
    pub current_change_number: Option<u64>,
    #[prost(message, repeated, tag = "2")]
    pub files: Vec<FileInfo>,
    #[prost(bool, optional, tag = "3")]
    pub is_only_delta: Option<bool>,
    #[prost(string, repeated, tag = "4")]
    pub path_prefixes: Vec<String>,
    #[prost(string, repeated, tag = "5")]
    pub machine_names: Vec<String>,
    #[prost(uint64, optional, tag = "6")]
    pub app_build_id: Option<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FileInfo {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(bytes = "vec", tag = "2")]
    pub sha: Vec<u8>,
    #[prost(uint64, tag = "3")]
    pub timestamp: u64,
    #[prost(uint32, tag = "4")]
    pub size: u32,
    #[prost(int32, tag = "5")]
    pub persist_state: i32,
    #[prost(uint32, tag = "6")]
    pub platforms: u32,
    #[prost(uint32, optional, tag = "7")]
    pub prefix_index: Option<u32>,
    #[prost(uint32, optional, tag = "8")]
    pub machine_index: Option<u32>,
    #[prost(bool, tag = "9")]
    pub reupload: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct QuotaResponse {
    #[prost(uint32, optional, tag = "1")]
    pub existing_files: Option<u32>,
    #[prost(uint64, optional, tag = "2")]
    pub existing_bytes: Option<u64>,
    #[prost(uint32, optional, tag = "3")]
    pub max_files: Option<u32>,
    #[prost(uint64, optional, tag = "4")]
    pub max_bytes: Option<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DownloadRequest {
    #[prost(uint32, tag = "1")]
    pub app_id: u32,
    #[prost(string, tag = "2")]
    pub name: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Header {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DownloadResponse {
    #[prost(uint32, optional, tag = "1")]
    pub app_id: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub file_size: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub raw_size: Option<u32>,
    #[prost(bytes = "vec", tag = "4")]
    pub sha: Vec<u8>,
    #[prost(uint64, tag = "5")]
    pub timestamp: u64,
    #[prost(bool, tag = "6")]
    pub deleted: bool,
    #[prost(string, tag = "7")]
    pub host: String,
    #[prost(string, tag = "8")]
    pub path: String,
    #[prost(bool, tag = "9")]
    pub https: bool,
    #[prost(message, repeated, tag = "10")]
    pub headers: Vec<Header>,
    #[prost(bool, tag = "11")]
    pub encrypted: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct BeginBatchRequest {
    #[prost(uint32, tag = "1")]
    pub app_id: u32,
    #[prost(string, tag = "2")]
    pub machine_name: String,
    #[prost(string, repeated, tag = "3")]
    pub uploads: Vec<String>,
    #[prost(string, repeated, tag = "4")]
    pub deletes: Vec<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct BeginBatchResponse {
    #[prost(uint64, optional, tag = "1")]
    pub batch_id: Option<u64>,
    #[prost(uint64, optional, tag = "4")]
    pub change_number: Option<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CompleteBatchRequest {
    #[prost(uint32, tag = "1")]
    pub app_id: u32,
    #[prost(uint64, tag = "2")]
    pub batch_id: u64,
    #[prost(uint32, tag = "3")]
    pub result: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct EmptyResponse {}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct BeginUploadRequest {
    #[prost(uint32, tag = "1")]
    pub app_id: u32,
    #[prost(uint32, tag = "2")]
    pub file_size: u32,
    #[prost(uint32, tag = "3")]
    pub raw_size: u32,
    #[prost(bytes = "vec", tag = "4")]
    pub sha: Vec<u8>,
    #[prost(uint64, tag = "5")]
    pub timestamp: u64,
    #[prost(string, tag = "6")]
    pub name: String,
    #[prost(uint32, tag = "7", default = "4294967295")]
    pub platforms: u32,
    #[prost(bool, tag = "10")]
    pub can_encrypt: bool,
    #[prost(uint64, tag = "13")]
    pub batch_id: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UploadBlock {
    #[prost(string, tag = "1")]
    pub host: String,
    #[prost(string, tag = "2")]
    pub path: String,
    #[prost(bool, tag = "3")]
    pub https: bool,
    #[prost(int32, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<Header>,
    #[prost(uint64, tag = "6")]
    pub offset: u64,
    #[prost(uint32, tag = "7")]
    pub length: u32,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub body: Option<Vec<u8>>,
    #[prost(bool, tag = "9")]
    pub may_parallelize: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct BeginUploadResponse {
    #[prost(bool, tag = "1")]
    pub encrypted: bool,
    #[prost(message, repeated, tag = "2")]
    pub blocks: Vec<UploadBlock>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CommitUploadRequest {
    #[prost(bool, tag = "1")]
    pub succeeded: bool,
    #[prost(uint32, tag = "2")]
    pub app_id: u32,
    #[prost(bytes = "vec", tag = "3")]
    pub sha: Vec<u8>,
    #[prost(string, tag = "4")]
    pub name: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CommitUploadResponse {
    #[prost(bool, optional, tag = "1")]
    pub committed: Option<bool>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DeleteRequest {
    #[prost(uint32, tag = "1")]
    pub app_id: u32,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(bool, tag = "3")]
    pub explicit: bool,
    #[prost(uint64, tag = "4")]
    pub batch_id: u64,
}
