// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// The relay page's test (pages/oauth/index.html), on the host with node and nothing else:
//
//   node vendor/sdv-everyware/everyware/pages/test/oauth.test.js
//
// It runs the page's script in a sandbox with a stand-in for the browser's location and
// document, and checks that the CSP's hash is the script's (else a browser would not run it).

'use strict';
const assert = require('assert');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const html = fs.readFileSync(path.join(__dirname, '..', 'oauth', 'index.html'), 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];

// The page as a browser runs it with `search` as its query: where it went and what it says.
function open(search) {
  const elements = { message: { textContent: '' }, continue: { hidden: true, href: '' } };
  const page = { went: null, elements };
  const sandbox = {
    URLSearchParams,
    location: { search, replace: (url) => { page.went = url; } },
    document: { getElementById: (id) => elements[id] },
  };
  // atob throws on what is not base64, as a browser's does
  sandbox.atob = (s) => {
    if (!/^[A-Za-z0-9+/]*={0,2}$/.test(s) || s.length % 4) throw new Error('InvalidCharacterError');
    return Buffer.from(s, 'base64').toString('latin1');
  };
  vm.runInNewContext(script, sandbox);
  page.relay = sandbox.relay;
  page.isLocal = sandbox.isLocal;
  return page;
}

const state = (obj) => Buffer.from(JSON.stringify(obj)).toString('base64url');
const query = (q) => '?' + new URLSearchParams(q).toString();
let checks = 0;
function check(name, fn) { fn(); checks++; console.log('ok ' + name); }

check('the CSP hash is the script\'s', () => {
  const hash = crypto.createHash('sha256').update(script, 'utf8').digest('base64');
  assert.ok(html.includes(`script-src 'sha256-${hash}'`), `script-src 'sha256-${hash}' not in the CSP`);
  assert.ok(html.includes('<meta name="referrer" content="no-referrer">'));
  assert.ok(!/<script[^>]+src=/.test(html), 'no script from elsewhere');
});

check('to the panel on the local network', () => {
  const s = state({ p: '192.168.1.23:8765', n: 'nonce-1' });
  const page = open(query({ code: '4/0AbCd+e&f', state: s, scope: 'https://www.googleapis.com/auth/calendar.events.readonly' }));
  const target = `http://192.168.1.23:8765/callback?code=${encodeURIComponent('4/0AbCd+e&f')}&state=${s}`;
  assert.strictEqual(page.went, target);
  assert.strictEqual(page.elements.continue.href, target);
  assert.strictEqual(page.elements.continue.hidden, false);
  assert.match(page.elements.message.textContent, /Continue/);
});

check('the state as the panel writes it (base64url without padding)', () => {
  // oauth::State::encode of {"p": "10.0.0.7:8765", "n": "abc-_1"}, from the Rust host test
  const s = Buffer.from('{"n":"abc-_1","p":"10.0.0.7:8765"}').toString('base64url');
  assert.strictEqual(open(query({ code: 'c', state: s })).went, `http://10.0.0.7:8765/callback?code=c&state=${s}`);
});

check('local addresses are taken', () => {
  const { isLocal } = open('');
  for (const host of ['10.0.0.1', '10.255.255.255', '172.16.0.1', '172.31.255.1', '192.168.0.1', '127.0.0.1', 'localhost', 'panel.local', 'my-panel.home.local']) {
    assert.ok(isLocal(host), host);
  }
  for (const host of ['8.8.8.8', '172.15.0.1', '172.32.0.1', '192.169.0.1', '11.0.0.1', '127.0.0.2', '0.0.0.0', '192.168.1.256', '192.168.01.1',
    '010.0.0.1', 'evil.com', 'local', '.local', 'panel.local.evil.com', '-x.local', '169.254.1.1', '100.64.0.1']) {
    assert.ok(!isLocal(host), host);
  }
});

check('anything else is not followed', () => {
  for (const p of ['evil.com:443', '8.8.8.8:80', '192.168.1.2', '192.168.1.2:0', '192.168.1.2:65536', '192.168.1.2:80/x', 'user@192.168.1.2:80',
    '192.168.1.2:80?x', '[::1]:80', '192.168.1.2:80#x', 'javascript:alert(1)', '']) {
    const page = open(query({ code: 'c', state: state({ p, n: 'n' }) }));
    assert.strictEqual(page.went, null, p);
    assert.strictEqual(page.elements.continue.hidden, true, p);
    assert.match(page.elements.message.textContent, /not lead to a panel/, p);
  }
  for (const s of ['not base64!', state({ n: 'n' }), state({ p: 7, n: 'n' }), 'A'.repeat(600), Buffer.from('[1,2]').toString('base64url')]) {
    assert.strictEqual(open(query({ code: 'c', state: s })).went, null, s);
  }
});

check('Google\'s error is shown, not followed', () => {
  const page = open(query({ error: 'access_denied', state: state({ p: '192.168.1.2:8765', n: 'n' }) }));
  assert.strictEqual(page.went, null);
  assert.match(page.elements.message.textContent, /\(access_denied\)/);
  // text only: the page sets textContent, never HTML
  const evil = open(query({ error: '<img src=x onerror=alert(1)>' + 'x'.repeat(100) }));
  assert.ok(evil.elements.message.textContent.includes('<img src=x onerror=alert(1)>'));
  assert.ok(evil.elements.message.textContent.length < 200);
});

check('without a code or a state', () => {
  for (const q of ['', '?code=c', '?state=s', '?code=&state=s']) {
    const page = open(q);
    assert.strictEqual(page.went, null, q);
    assert.match(page.elements.message.textContent, /started on your panel/, q);
  }
});

console.log(`${checks} checks passed`);
