//! The OAuth2 link a host opens to add the bot to a server.

use twilight_model::guild::Permissions;

/// What the bot needs: read and write in channels and threads, react, pin
/// and post polls. Embed Links is left off because the agent only writes
/// plain text. Pinning needs Pin Messages; Manage Messages is kept for
/// servers that still tie pinning to it.
pub const INVITE_PERMISSIONS: Permissions = Permissions::VIEW_CHANNEL
    .union(Permissions::SEND_MESSAGES)
    .union(Permissions::SEND_MESSAGES_IN_THREADS)
    .union(Permissions::READ_MESSAGE_HISTORY)
    .union(Permissions::ADD_REACTIONS)
    .union(Permissions::MANAGE_MESSAGES)
    .union(Permissions::PIN_MESSAGES)
    .union(Permissions::SEND_POLLS);

/// The invite link for an application's client id. Anything but digits is
/// dropped from the id.
pub fn invite_url(client_id: &str) -> String {
    let id: String = client_id.chars().filter(char::is_ascii_digit).collect();
    format!(
        "https://discord.com/oauth2/authorize?client_id={id}&scope=bot&permissions={}",
        INVITE_PERMISSIONS.bits()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_bits_are_exactly_what_the_bot_needs() {
        let expected = (1 << 10) // view channels
            | (1 << 11) // send messages
            | (1 << 38) // send messages in threads
            | (1 << 16) // read message history
            | (1 << 6) // add reactions
            | (1 << 13) // manage messages
            | (1 << 51) // pin messages
            | (1 << 49); // create polls
        assert_eq!(INVITE_PERMISSIONS.bits(), expected);
        assert_eq!(INVITE_PERMISSIONS.bits(), 2_815_024_645_090_368);
        assert!(!INVITE_PERMISSIONS.contains(Permissions::EMBED_LINKS));
        assert!(!INVITE_PERMISSIONS.contains(Permissions::ADMINISTRATOR));
    }

    #[test]
    fn the_link_carries_the_client_id_scope_and_bits() {
        let url = invite_url("123456789012345678");
        assert_eq!(
            url,
            format!(
                "https://discord.com/oauth2/authorize?client_id=123456789012345678&scope=bot&permissions={}",
                INVITE_PERMISSIONS.bits()
            )
        );
    }

    #[test]
    fn only_digits_survive_in_the_client_id() {
        assert!(invite_url("12 34&scope=admin").contains("client_id=1234&scope=bot"));
    }
}
