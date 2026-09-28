// Keep the pinned MCP server in this managed process and on its original stdio.
const path = require('node:path');

const cli = path.join(__dirname, 'node_modules/@playwright/mcp/cli.js');
const config = path.join(__dirname, 'browser.json');
const artifacts = path.join(__dirname, 'artifacts');

if (process.platform !== 'win32') {
  const executionTemp = process.env.TMPDIR;
  if (!executionTemp || !path.isAbsolute(executionTemp)) {
    throw new Error('The browser workflow requires the managed process TMPDIR.');
  }
  // Unix socket limits apply to the pathname passed to bind, not the directory's
  // absolute path. Both SDK endpoints stay in this already granted generation.
  process.chdir(executionTemp);
  process.env.PWTEST_SOCKETS_DIR = '.';
}

process.argv = [
  process.execPath,
  cli,
  '--config', config,
  '--output-dir', artifacts,
  '--headless',
  '--isolated',
  '--no-webmcp',
];
require(cli);
