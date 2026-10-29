import re

with open('ui/src/onboarding.rs', 'r') as f:
    content = f.read()

content = re.sub(
    r'\.text_color\(rgb\(if unsupported \{\n.*?MUTED\n.*?\} else if selected \{',
    r'.text_color(rgb(if selected {',
    content,
    flags=re.DOTALL
)

with open('ui/src/onboarding.rs', 'w') as f:
    f.write(content)
