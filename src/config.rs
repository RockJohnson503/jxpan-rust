use std::net::{AddrParseError, SocketAddr};

const DEFAULT_LISTEN_ADDRESS: &str = "127.0.0.1:8789";

pub(crate) fn listen_address(value: Option<&str>) -> Result<SocketAddr, AddrParseError> {
    value.unwrap_or(DEFAULT_LISTEN_ADDRESS).parse()
}

#[cfg(test)]
mod tests {
    use super::listen_address;

    #[test]
    fn defaults_to_loopback_without_using_the_existing_service_port() {
        let address = listen_address(None).unwrap();
        assert!(address.ip().is_loopback());
        assert_eq!(address.port(), 8789);
    }

    #[test]
    fn accepts_ipv4_with_an_ephemeral_port() {
        let address = listen_address(Some("127.0.0.1:0")).unwrap();
        assert!(address.is_ipv4());
        assert_eq!(address.port(), 0);
    }

    #[test]
    fn accepts_ipv6() {
        let address = listen_address(Some("[::1]:8789")).unwrap();
        assert!(address.is_ipv6());
        assert!(address.ip().is_loopback());
    }

    #[test]
    fn rejects_empty_invalid_and_out_of_range_addresses() {
        for value in ["", "localhost:8789", "127.0.0.1", "127.0.0.1:65536"] {
            assert!(listen_address(Some(value)).is_err(), "accepted {value:?}");
        }
    }
}
