import test from 'node:test';
import assert from 'node:assert/strict';
import { managementClientAllowed } from '../dist/management/http.js';

function request(remoteAddress, host = '127.0.0.1:3789') {
  return { socket: { remoteAddress }, headers: { host } };
}

test('management client policy keeps loopback-only by default and permits only private bridge peers when explicitly trusted', () => {
  assert.equal(managementClientAllowed(request('127.0.0.1'), false), true);
  assert.equal(managementClientAllowed(request('172.17.0.1'), false), false);
  assert.equal(managementClientAllowed(request('172.17.0.1'), true), true);
  assert.equal(managementClientAllowed(request('10.0.0.1'), true), true);
  assert.equal(managementClientAllowed(request('192.168.1.1'), true), true);
  assert.equal(managementClientAllowed(request('8.8.8.8'), true), false);
  assert.equal(managementClientAllowed(request('172.17.0.1', '192.168.1.10:3789'), true), false);
});
