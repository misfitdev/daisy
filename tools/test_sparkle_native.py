"""Run isolated native update hosts; never replace the installed application."""
import argparse
import base64
import functools
import hashlib
import http.server
import os
from pathlib import Path
import plistlib
import re
import shutil
import signal
import subprocess
import tempfile
import threading
import time
import uuid
import xml.etree.ElementTree as ET
import sparkle
import sparkle_appcast as feed

ROOT = Path(__file__).resolve().parents[1]
from sparkle_fixtures import INSTALLED_VERSION, UPDATE_VERSION, SUPPORTED_PROTOCOLS, REPLACEMENT_STARTED, ROUTINE_CHANNEL


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass


def fixture_key(work):
    # A new ephemeral signing key for this fixture, unrelated to production keys.
    pem = work / 'private.pem'
    subprocess.run(['openssl', 'genpkey', '-algorithm', 'ED25519', '-out', str(pem)], check=True, stdout=subprocess.DEVNULL)
    seed = subprocess.check_output(['openssl', 'pkey', '-in', str(pem), '-outform', 'DER'])[-32:]
    public = subprocess.check_output(['openssl', 'pkey', '-in', str(pem), '-pubout', '-outform', 'DER'])[-32:]
    key = work / 'fixture-key'
    key.write_bytes(base64.b64encode(seed))
    key.chmod(0o600)
    return key, base64.b64encode(public).decode()


def verify_result(case, installed, text):
    version = plistlib.loads((installed/'Contents/Info.plist').read_bytes())['CFBundleVersion']
    if case in ('good', 'relaunch', 'relaunch-retry'):
        assert version == UPDATE_VERSION and REPLACEMENT_STARTED in text, 'native installation and reopen did not complete'
        if case in ('relaunch', 'relaunch-retry'):
            assert 'restored selected profile' in text, 'native relaunch lost the selected profile'
        if case == 'relaunch-retry':
            assert 'retried relaunch after save failure' in text, 'postponed update was not retried'
        subprocess.run(['codesign','--verify','--deep','--strict',str(installed)],check=True)
    elif case == 'manual':
        assert version == INSTALLED_VERSION and 'did-find-valid-update' in text, text
    else:
        assert version == INSTALLED_VERSION and 'did-abort-with-error' in text, 'update was not rejected'


def run_case(case, identity):
    with tempfile.TemporaryDirectory(prefix='daisy-native-update-') as directory:
        work = Path(directory)
        key, public = fixture_key(work)
        identifier = 'dev.misfit.daisy.update-test.' + uuid.uuid4().hex
        log = work / 'events.log'
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(work)))
        threading.Thread(target=server.serve_forever, daemon=True).start()
        url = f'http://127.0.0.1:{server.server_port}'
        installed = work / 'installed/Daisy.app'
        candidate = work / 'candidate/Daisy.app'
        for app, version in [(installed, INSTALLED_VERSION), (candidate, UPDATE_VERSION)]:
            (app / 'Contents/MacOS').mkdir(parents=True)
            shutil.copy2(ROOT / 'target/debug/examples/sparkle_smoke', app / 'Contents/MacOS/sparkle-smoke')
            info = dict(CFBundleExecutable='sparkle-smoke', CFBundleIdentifier=identifier, CFBundleName='Daisy Update Test',
                        CFBundlePackageType='APPL', CFBundleVersion=version, CFBundleShortVersionString=version,
                        LSUIElement=True, SUFeedURL=url+'/appcast.xml', SUPublicEDKey=public,
                        SURequireSignedFeed=True, SUVerifyUpdateBeforeExtraction=True,
                        SUEnableAutomaticChecks=True, SUAutomaticallyUpdate=True,
                        NSAppTransportSecurity=dict(NSAllowsArbitraryLoads=True), DaisySmokeLog=str(log),
                        DaisySmokeChannel=ROUTINE_CHANNEL if case == "incompatible" else "stable",
                        DaisySmokeReplacement=app == candidate,
                DaisySmokeManual=case == 'manual',
                DaisySmokeRelaunch=case in ('relaunch', 'relaunch-retry'),
                DaisySmokeRetry=case == 'relaunch-retry',
                DaisySmokeFixtureUser=str(work/'fixture-user'),
                DaisySmokeHome=str(work/'selected-profile'),
                DaisySmokeRelaunchRoot=str(work/'fixture-user/Library/Application Support/daisy'))
            (app/'Contents/Info.plist').write_bytes(plistlib.dumps(info))
            framework = app/'Contents/Frameworks/Sparkle.framework'
            framework.parent.mkdir()
            shutil.copytree(ROOT/'target/sparkle/Sparkle.framework', framework, symlinks=True)
            sparkle.sign(app, identity)
            command = ['codesign', '--force', '--options', 'runtime', '--sign', identity]
            if identity == '-':
                command += ['--entitlements', str(ROOT/'macos/dev-entitlements.plist')]
            else:
                command += ['--timestamp']
            subprocess.run(command+[str(app)], check=True)
            subprocess.run(['codesign','--verify','--deep','--strict',str(app)],check=True)
        archive = work/f'Daisy-{UPDATE_VERSION}-macos-arm64.zip'
        subprocess.run(['ditto','-c','-k','--keepParent',str(candidate),str(archive)],check=True)
        signature = subprocess.check_output([str(feed.SIGNER),'--ed-key-file',str(key),'-p',str(archive)],text=True).strip()
        xml = ET.fromstring('<rss version="2.0"><channel><title>Daisy Updates</title></channel></rss>')
        incompatible = case in ('incompatible', 'manual')
        feed.add_release(xml, dict(version=UPDATE_VERSION,protocol=8 if incompatible else max(SUPPORTED_PROTOCOLS),supported_protocols=[7,8] if incompatible else SUPPORTED_PROTOCOLS), archive.stat().st_size,signature)
        for enclosure in xml.findall('channel/item/enclosure'):
            enclosure.set('url',url+'/'+archive.name)
        cast = work/'appcast.xml'
        ET.ElementTree(xml).write(cast,encoding='utf-8',xml_declaration=True)
        subprocess.run([str(feed.SIGNER),'--ed-key-file',str(key),str(cast)],check=True)
        if case == 'bad-feed':
            cast.write_bytes(cast.read_bytes().replace(b'Daisy Updates',b'Changed Updates'))
        if case == 'bad-archive':
            with archive.open('ab') as changed:
                changed.write(b'altered after signing')
        try:
            if case == 'relaunch-retry':
                (work/'fixture-user').write_text('block the handoff directory')
            command = ['open','-n','-g']
            if case in ('relaunch', 'relaunch-retry'):
                command += ['--env', 'HOME='+str(work/'fixture-user')]
            command += [str(installed)]
            if case in ('relaunch', 'relaunch-retry'):
                command += ['--args', '--home', str(work/'selected-profile')]
            subprocess.run(command,check=True)
            reopened = False
            deadline = time.monotonic()+60
            while time.monotonic()<deadline:
                text = log.read_text() if log.exists() else ''
                current = plistlib.loads((installed/'Contents/Info.plist').read_bytes())['CFBundleVersion']
                if case == 'good' and current == UPDATE_VERSION and not reopened:
                    subprocess.run(['open','-n','-g',str(installed)],check=True)
                    reopened = True
                completed = 'restored selected profile' in text if case in ('relaunch', 'relaunch-retry') else REPLACEMENT_STARTED in text
                if completed or 'did-abort-with-error' in text or 'timeout' in text or (case == 'manual' and 'did-find-valid-update' in text):
                    break
                time.sleep(0.2)
            text = log.read_text() if log.exists() else ''
            print(case+':\n'+text,flush=True)
            verify_result(case, installed, text)
        finally:
            # Terminate only processes identified by this fixture's private log.
            if log.exists():
                for pid in re.findall(r'^started [^\n]+ pid=(\d+)$',log.read_text(),re.MULTILINE):
                    try:
                        os.kill(int(pid),signal.SIGTERM)
                    except ProcessLookupError:
                        pass
            server.shutdown()
            subprocess.run(['defaults','delete',identifier],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--identity',default='-')
    parser.add_argument('--case',choices=['good','bad-feed','bad-archive','incompatible','manual','relaunch','relaunch-retry'])
    args=parser.parse_args()
    identity=args.identity
    if identity == 'developer-id':
        result=subprocess.check_output(['security','find-identity','-v','-p','codesigning'],text=True)
        identity=re.search(r'([0-9A-F]{40}) "Developer ID Application:',result)[1]
    for case in ([args.case] if args.case else ['good','bad-feed','bad-archive','incompatible','manual','relaunch','relaunch-retry']):
        run_case(case,identity)
