import json
import subprocess
import sys
import tarfile
from pathlib import Path

root = Path(sys.argv[1])
archive = Path(sys.argv[2])
environment = {}
for name, command in [('os', ['cat', '/etc/os-release']), ('kernel', ['uname', '-a']), ('pacman', ['pacman', '--version']), ('makepkg', ['makepkg', '--version']), ('kitty', ['kitty', '--version']), ('user', ['id'])]:
    result = subprocess.run(command, capture_output=True, text=True)
    environment[name] = {'status': result.returncode, 'output': result.stdout}
(root / 'environment.json').write_text(json.dumps(environment, indent=2))
with tarfile.open(archive, 'w:gz') as output:
    for path in root.rglob('*'):
        if path.is_file() and (path.suffix == '.json' or path.name.endswith(('.screen.txt', '.raw.txt', '.status', '.term-before', '.term-after', '.log')) or path.name == 'redirected.txt'):
            output.add(path, arcname=str(path.relative_to(root)), recursive=False)
print(json.dumps({'archive': str(archive), 'bytes': archive.stat().st_size}))
