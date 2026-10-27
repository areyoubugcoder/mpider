// 会话观察器去重回归：相同封装前缀不能吞掉另一公众号的完整候选。
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const source = fs.readFileSync(new URL('../native/session_agent.js', import.meta.url), 'utf8');
function agent() {
  const events = [];
  const context = vm.createContext({ rpc: { exports: {} }, send: event => events.push(event) });
  vm.runInContext(source, context);
  return { context, events };
}
test('持续观察不混淆长度及前缀相同的不同公众号模板', () => {
  const { context, events } = agent();
  vm.runInContext(`sessionContinuous=true; sessionCandidate('aa'.repeat(32)+'01',33); sessionCandidate('aa'.repeat(32)+'02',33); sessionCandidate('aa'.repeat(32)+'01',33);`, context);
  assert.equal(events.length, 2);
  assert.notEqual(events[0].hex, events[1].hex);
});
test('持续观察超过32个模板仍继续交付，有界缓存会释放旧候选', () => {
  const { context, events } = agent();
  vm.runInContext(`sessionContinuous=true; for(let i=0;i<40;i++) sessionCandidate('aa'.repeat(32)+i.toString(16).padStart(2,'0'),33);`, context);
  assert.equal(events.length, 40);
  assert.equal(vm.runInContext('candidateSignatures.size', context), 32);
  vm.runInContext(`sessionCandidate('aa'.repeat(32)+'00',33);`, context);
  assert.equal(events.length, 41);
});
