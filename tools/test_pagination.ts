/** 全局分页契约：空/单页也能显示总数，页码窗口与第一页/最后一页边界稳定。 */
import test from 'node:test';
import assert from 'node:assert/strict';
import { DEFAULT_PAGE_SIZE, pageCountFor, pageNumbers } from '../frontend/src/lib/pagination.ts';
test('默认每页10条，零条/少于一页也保留一页',()=>{assert.equal(DEFAULT_PAGE_SIZE,10);assert.equal(pageCountFor(0,10),1);assert.equal(pageCountFor(9,10),1);assert.equal(pageCountFor(21,10),3);});
test('小列表显示全部页码，大列表保留当前页和首尾跳转',()=>{assert.deepEqual(pageNumbers(0,3),[0,1,2]);assert.deepEqual(pageNumbers(50,101),[0,'ellipsis',49,50,51,'ellipsis',100]);});
test('越界页码被夹在有效范围',()=>{assert.deepEqual(pageNumbers(-2,0),[0]);assert.deepEqual(pageNumbers(1000,9),[0,'ellipsis',7,8]);});
