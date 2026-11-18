use bincode::error::DecodeError;
use flate2::bufread::ZlibDecoder;
use serde::de::DeserializeOwned;

pub fn from_uncompressed_data<T: DeserializeOwned>(data: &[u8]) -> Result<T, DecodeError> {
    bincode::serde::decode_from_slice(data, bincode::config::legacy()).map(|(value, _)| value)
}

pub(crate) fn from_compressed_data<T: DeserializeOwned>(input: &[u8]) -> Result<T, DecodeError> {
    let mut decoder = ZlibDecoder::new(input);
    bincode::serde::decode_from_std_read(&mut decoder, bincode::config::legacy())
}
