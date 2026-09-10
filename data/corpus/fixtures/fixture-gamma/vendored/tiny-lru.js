// SPDX-License-Identifier: MIT
// Vendored tiny LRU used by the dedup-control fixture pair.
"use strict";

class TinyLru {
  constructor(capacity) {
    if (!Number.isInteger(capacity) || capacity <= 0) {
      throw new RangeError("capacity must be a positive integer");
    }
    this.capacity = capacity;
    this.size = 0;
    this.head = null;
    this.tail = null;
    this.map = new Map();
  }

  get(key) {
    const node = this.map.get(key);
    if (!node) return undefined;
    this.detach(node);
    this.pushFront(node);
    return node.value;
  }

  has(key) {
    return this.map.has(key);
  }

  set(key, value) {
    let node = this.map.get(key);
    if (node) {
      node.value = value;
      this.detach(node);
      this.pushFront(node);
      return this;
    }
    node = { key, value, prev: null, next: null };
    this.map.set(key, node);
    this.pushFront(node);
    this.size += 1;
    if (this.size > this.capacity) {
      this.evict();
    }
    return this;
  }

  delete(key) {
    const node = this.map.get(key);
    if (!node) return false;
    this.detach(node);
    this.map.delete(key);
    this.size -= 1;
    return true;
  }

  evict() {
    const victim = this.tail;
    if (!victim) return;
    this.detach(victim);
    this.map.delete(victim.key);
    this.size -= 1;
  }

  detach(node) {
    if (node.prev) node.prev.next = node.next;
    else this.head = node.next;
    if (node.next) node.next.prev = node.prev;
    else this.tail = node.prev;
    node.prev = null;
    node.next = null;
  }

  pushFront(node) {
    node.next = this.head;
    node.prev = null;
    if (this.head) this.head.prev = node;
    this.head = node;
    if (!this.tail) this.tail = node;
  }

  keys() {
    const out = [];
    let cur = this.head;
    while (cur) {
      out.push(cur.key);
      cur = cur.next;
    }
    return out;
  }

  clear() {
    this.map.clear();
    this.head = null;
    this.tail = null;
    this.size = 0;
  }
}

module.exports = TinyLru;
