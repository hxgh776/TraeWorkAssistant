import { describe, expect, it } from 'vitest';
import { isExpiredAccount } from './accountStatusModel';

const NOW = 1_700_000_000_000;

describe('isExpiredAccount（VENU-41 F1：概述与签到页同口径）', () => {
  it('needs_relogin 标记 → 失效（即使 token 未到期）', () => {
    expect(isExpiredAccount({ needs_relogin: true, token_expires_at_ms: NOW + 3_600_000 }, NOW)).toBe(true);
    expect(isExpiredAccount({ needs_relogin: true, token_expires_at_ms: 0 }, NOW)).toBe(true);
  });

  it('token 有明确到期时刻且已过 → 失效（与签到页「已过期」徽标同判）', () => {
    expect(isExpiredAccount({ needs_relogin: false, token_expires_at_ms: NOW - 1 }, NOW)).toBe(true);
    expect(isExpiredAccount({ needs_relogin: false, token_expires_at_ms: NOW }, NOW)).toBe(true);
  });

  it('token 未到期 → 不失效', () => {
    expect(isExpiredAccount({ needs_relogin: false, token_expires_at_ms: NOW + 1 }, NOW)).toBe(false);
  });

  it('token 到期时刻未知（0/缺省/null）→ 不失效（可自愈账号不得误报）', () => {
    expect(isExpiredAccount({ needs_relogin: false, token_expires_at_ms: 0 }, NOW)).toBe(false);
    expect(isExpiredAccount({ needs_relogin: false, token_expires_at_ms: null }, NOW)).toBe(false);
    expect(isExpiredAccount({ needs_relogin: false }, NOW)).toBe(false);
  });

  it('混合态（1 失效 + 1 正常）计数 = 1，失效者被精确识别', () => {
    const accounts = [
      { needs_relogin: false, token_expires_at_ms: NOW - 1_000 }, // 已过期
      { needs_relogin: false, token_expires_at_ms: NOW + 3_600_000 }, // 正常
    ];
    const expired = accounts.filter((a) => isExpiredAccount(a, NOW));
    expect(expired).toHaveLength(1);
    expect(expired[0].token_expires_at_ms).toBe(NOW - 1_000);
  });

  it('混合态（needs_relogin 标记 + token 未知）计数 = 1', () => {
    const accounts = [
      { needs_relogin: true, token_expires_at_ms: null }, // 标记失效
      { needs_relogin: false }, // 正常（未知到期）
    ];
    const expired = accounts.filter((a) => isExpiredAccount(a, NOW));
    expect(expired).toHaveLength(1);
    expect(expired[0].needs_relogin).toBe(true);
  });
});
