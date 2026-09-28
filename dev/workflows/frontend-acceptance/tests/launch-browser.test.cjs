const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const { once } = require('node:events');
const fs = require('node:fs/promises');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const readline = require('node:readline');
const { test } = require('node:test');
const { pathToFileURL } = require('node:url');

// Explicit overrides select test fixtures only; the shipping launcher has none.
const source = path.resolve(process.env.C1_WORKFLOW_SOURCE || path.join(__dirname, '..'));
const dependencies = path.resolve(process.env.C1_TEST_DEPENDENCIES || path.join(source, 'node_modules'));

function connect(process, workspace) {
  const pending = new Map();
  let sequence = 0;
  let rootsRequested = false;
  let stderr = '';
  process.stderr.on('data', bytes => { stderr = (stderr + bytes).slice(-65536); });
  const lines = readline.createInterface({ input: process.stdout });
  const write = message => process.stdin.write(`${JSON.stringify(message)}\n`);
  lines.on('line', line => {
    const message = JSON.parse(line);
    if (message.method === 'roots/list' && message.id !== undefined) {
      rootsRequested = true;
      write({ jsonrpc: '2.0', id: message.id, result: {
        roots: [{ uri: pathToFileURL(workspace).href, name: 'original workspace' }],
      } });
    } else if (message.method === 'ping' && message.id !== undefined) {
      write({ jsonrpc: '2.0', id: message.id, result: {} });
    } else if (pending.has(message.id)) {
      const { resolve, reject, timer } = pending.get(message.id);
      pending.delete(message.id);
      clearTimeout(timer);
      if (message.error) reject(new Error(JSON.stringify(message.error)));
      else if (message.result?.isError) reject(new Error(JSON.stringify(message.result)));
      else resolve(message.result);
    }
  });
  process.on('close', () => {
    for (const { reject, timer } of pending.values()) {
      clearTimeout(timer);
      reject(new Error(`MCP closed before response: ${stderr}`));
    }
    pending.clear();
  });
  return {
    request(method, params) {
      return new Promise((resolve, reject) => {
        const id = ++sequence;
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error(`MCP deadline for ${method}: ${stderr}`));
        }, 30000);
        pending.set(id, { resolve, reject, timer });
        write({ jsonrpc: '2.0', id, method, params });
      });
    },
    notify(method) { write({ jsonrpc: '2.0', method }); },
    rootsRequested: () => rootsRequested,
    finish() { lines.close(); },
  };
}

async function closeProcess(child, closed) {
  child.stdin.end();
  let timer;
  try {
    const result = await Promise.race([
      closed,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('MCP did not close after stdin EOF')), 15000);
      }),
    ]);
    assert.deepEqual(result, [0, null], 'cooperative MCP exit');
  } catch (error) {
    child.kill('SIGTERM');
    const force = setTimeout(() => child.kill('SIGKILL'), 2000);
    await closed;
    clearTimeout(force);
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

test('long managed temporary path keeps browser sockets and original workspace boundaries', {
  skip: process.platform === 'win32' && 'Unix pathname limit; Windows uses unchanged named pipes',
  timeout: 90000,
}, async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'sigil-browser-workflow-'));
  let child;
  let closed;
  let client;
  let server;
  try {
    const workspace = path.join(root, 'workspace');
    const installed = path.join(workspace, '.sigil/plugins/frontend-acceptance');
    const home = path.join(root, 'home');
    const temporary = path.join(root, 't'.repeat(Math.max(1, 260 - Buffer.byteLength(root) - 1)));
    await fs.mkdir(installed, { recursive: true, mode: 0o700 });
    await fs.mkdir(home, { mode: 0o700 });
    await fs.mkdir(temporary, { mode: 0o700 });
    assert.ok(Buffer.byteLength(temporary) >= 240, 'fixture exceeds the SDK socket budget');
    for (const name of ['launch-browser.cjs', 'browser.json']) {
      await fs.copyFile(path.join(source, name), path.join(installed, name));
    }
    await fs.symlink(dependencies, path.join(installed, 'node_modules'), 'dir');
    const configPath = path.join(installed, 'browser.json');
    const config = JSON.parse(await fs.readFile(configPath, 'utf8'));
    const executablePath = process.env.C1_TEST_BROWSER_EXECUTABLE || config.browser.launchOptions.executablePath;
    assert.ok(executablePath && path.isAbsolute(executablePath), 'explicitly configure an installed browser');
    assert.ok((await fs.stat(executablePath)).isFile());
    config.browser.launchOptions.executablePath = executablePath;
    await fs.writeFile(configPath, JSON.stringify(config));
    server = http.createServer((_request, response) => {
      response.writeHead(200, { 'Content-Type': 'text/html' });
      response.end('<html><title>Managed workflow path</title><body><h1>Original workspace retained</h1></body></html>');
    });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    child = spawn(process.execPath, [path.join(installed, 'launch-browser.cjs')], {
      cwd: installed,
      env: { PATH: process.env.PATH || '/usr/bin:/bin', HOME: home,
        TMPDIR: temporary, TMP: temporary, TEMP: temporary, LANG: 'en_US.UTF-8' },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    closed = once(child, 'close');
    client = connect(child, workspace);
    await client.request('initialize', { protocolVersion: '2024-11-05',
      capabilities: { roots: { listChanged: true } },
      clientInfo: { name: 'Sigil workflow regression', version: '1' } });
    client.notify('notifications/initialized');
    const tools = await client.request('tools/list', {});
    assert.ok(tools.tools.some(tool => tool.name === 'browser_navigate'));
    const call = (name, args) => client.request('tools/call', { name, arguments: args });
    const result = await call('browser_navigate', { url: `http://127.0.0.1:${server.address().port}/` });
    assert.match(JSON.stringify(result), /Managed workflow path/);
    assert.equal(client.rootsRequested(), true);
    const sockets = (await fs.readdir(path.join(temporary, 'browser'))).filter(name => name.endsWith('.sock'));
    assert.equal(sockets.length, 1);
    const socket = path.join(temporary, 'browser', sockets[0]);
    assert.equal((await fs.lstat(socket)).isSocket(), true, 'socket remains inside this process temporary directory');
    await call('browser_take_screenshot', { filename: 'relative-screenshot.png' });
    assert.ok((await fs.stat(path.join(workspace, 'relative-screenshot.png'))).isFile());
    await call('browser_take_screenshot', {});
    assert.ok((await fs.readdir(path.join(installed, 'artifacts'))).some(name => name.endsWith('.png')));
    await assert.rejects(call('browser_take_screenshot', { filename: path.join(root, 'outside.png') }), /outside allowed roots/);
    await call('browser_close', {});
    await closeProcess(child, closed);
    child = null;
    await assert.rejects(fs.lstat(socket), { code: 'ENOENT' });
  } finally {
    if (child) await closeProcess(child, closed);
    client?.finish();
    if (server) await new Promise(resolve => server.close(resolve));
    await fs.rm(root, { recursive: true, force: true });
  }
});
