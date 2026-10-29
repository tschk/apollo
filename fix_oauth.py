import re

def remove_dead_code(content):
    # Removing Support variants
    content = re.sub(
        r'    /// Sign-in works; apollo needs a non-default build to use it\.\n    #\[allow\(dead_code\)\]\n    NeedsFeature\(&\'static str\),\n',
        '',
        content
    )
    content = re.sub(
        r'    /// apollo cannot use this login; sign-in is not offered\. \(No provider\n    /// is in this state today; kept so the UI handles one honestly\.\)\n    #\[allow\(dead_code\)\]\n    Unsupported\(&\'static str\),\n',
        '',
        content
    )

    # Removing callback_port
    content = re.sub(
        r'    /// Local port the provider\'s redirect comes back to\.\n    /// \(`rs_ai_oauth` keeps its own accessor private; these match its\n    /// documented redirect URIs\.\)\n    #\[allow\(dead_code\)\]\n    pub fn callback_port\(self\) -> u16 \{\n        match self \{\n            OAuthKind::ChatGpt => 1455,\n            OAuthKind::Copilot => 9876,\n            OAuthKind::Claude => 53692,\n        \}\n    \}\n',
        '',
        content
    )

    # Removing if let Support::Unsupported(why) = self.support()
    content = re.sub(
        r'        if let Support::Unsupported\(why\) = self\.support\(\) \{\n            return Err\(why\.to_string\(\)\);\n        \}\n',
        '',
        content
    )

    return content

with open('ui/src/oauth.rs', 'r') as f:
    content = f.read()

with open('ui/src/oauth.rs', 'w') as f:
    f.write(remove_dead_code(content))
