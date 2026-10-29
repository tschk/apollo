import re

def remove_dead_code(content):
    # Auth::OAuth(kind) if matches!(kind.support(), Support::Unsupported(_)) => vec![],
    content = re.sub(
        r'                Auth::OAuth\(kind\) if matches!\(kind\.support\(\), Support::Unsupported\(_\)\) => vec!\[\],\n',
        '',
        content
    )

    # if let Support::Unsupported(why) = kind.support() { ... }
    content = re.sub(
        r'                if let Support::Unsupported\(why\) = kind\.support\(\) \{\n                    return Err\(\(\n                        format!\("\{\} can\'t be used: \{why\}", self\.provider\.label\),\n                        None,\n                    \)\);\n                \}\n',
        '',
        content
    )

    # if matches!(kind.support(), Support::Unsupported(_)) { return; }
    content = re.sub(
        r'        if matches!\(kind\.support\(\), Support::Unsupported\(_\)\) \{\n            return;\n        \}\n',
        '',
        content
    )

    # Support::Unsupported(_) => ("not supported by apollo".into(), GHOST),
    content = re.sub(
        r'            Support::Unsupported\(_\) => \("not supported by apollo"\.into\(\), GHOST\),\n',
        '',
        content
    )
    # Support::NeedsFeature(_) if self.signed_in[i] => ("signed in".into(), SUCCESS),
    content = re.sub(
        r'            Support::NeedsFeature\(_\) if self\.signed_in\[i\] => \("signed in"\.into\(\), SUCCESS\),\n',
        '',
        content
    )
    # Support::NeedsFeature(_) => ("not available in this build".into(), MUTED),
    content = re.sub(
        r'            Support::NeedsFeature\(_\) => \("not available in this build"\.into\(\), MUTED\),\n',
        '',
        content
    )

    # let unsupported = matches!(kind.support(), Support::Unsupported(_));
    content = re.sub(
        r'                let unsupported = matches!\(kind\.support\(\), Support::Unsupported\(_\)\);\n',
        '',
        content
    )
    # _, Support::Unsupported(why) => (why.to_string(), MUTED),
    content = re.sub(
        r'                    \(_, Support::Unsupported\(why\)\) => \(why\.to_string\(\), MUTED\),\n',
        '',
        content
    )
    # (_, Support::NeedsFeature(_)) if self.signed_in[i] => ( ... ),
    content = re.sub(
        r'                    \(_, Support::NeedsFeature\(_\)\) if self\.signed_in\[i\] => \(\n                        "signed in, but this build cannot use that login"\.into\(\),\n                        WARN,\n                    \),\n',
        '',
        content
    )
    # (_, Support::NeedsFeature(_)) => ("not available in this build".into(), MUTED),
    content = re.sub(
        r'                    \(_, Support::NeedsFeature\(_\)\) => \("not available in this build"\.into\(\), MUTED\),\n',
        '',
        content
    )
    # Support::Unsupported(_) => None,
    content = re.sub(
        r'                    Support::Unsupported\(_\) => None,\n',
        '',
        content
    )

    # .when(!unsupported, |d| d.child(self.model_field(cx))) -> .child(self.model_field(cx))
    content = re.sub(
        r'\.when\(!unsupported, \|d\| d\.child\(self\.model_field\(cx\)\)\)',
        r'.child(self.model_field(cx))',
        content
    )

    return content

with open('ui/src/onboarding.rs', 'r') as f:
    content = f.read()

with open('ui/src/onboarding.rs', 'w') as f:
    f.write(remove_dead_code(content))
