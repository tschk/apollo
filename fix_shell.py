import re

def remove_dead_code(content):
    content = re.sub(
        r'        if matches!\(kind\.support\(\), crate::oauth::Support::Unsupported\(_\)\) \{\n            self\.notice = "that sign-in is not available"\.into\(\);\n            cx\.notify\(\);\n            return;\n        \}\n',
        '',
        content
    )
    return content

with open('ui/src/shell.rs', 'r') as f:
    content = f.read()

with open('ui/src/shell.rs', 'w') as f:
    f.write(remove_dead_code(content))
