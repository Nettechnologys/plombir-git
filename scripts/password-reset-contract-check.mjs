#!/usr/bin/env node

import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const resetPagePath = path.join(root, 'web/src/routes/reset-password/+page.svelte');
const forgotPagePath = path.join(root, 'web/src/routes/forgot-password/+page.svelte');
const loginPagePath = path.join(root, 'web/src/routes/login/+page.svelte');
const passwordValidatorPath = path.join(root, 'crates/rg-core/src/auth/password.rs');
const userServicePath = path.join(root, 'crates/rg-core/src/user/service.rs');

const resetPage = readFileSync(resetPagePath, 'utf8');
const forgotPage = existsSync(forgotPagePath) ? readFileSync(forgotPagePath, 'utf8') : '';
const loginPage = readFileSync(loginPagePath, 'utf8');
const passwordValidator = readFileSync(passwordValidatorPath, 'utf8');
const userService = readFileSync(userServicePath, 'utf8');

const failures = [];

function expect(source, pattern, message) {
  if (!pattern.test(source)) failures.push(message);
}

expect(
  userService,
  /reset_password[\s\S]*PasswordValidator::standard\(\)[\s\S]*validate_with_username\(new_password,\s*&user\.username\)/,
  'Backend reset_password must keep using the standard password validator',
);

expect(passwordValidator, /min_length:\s*8/, 'Backend standard password validator must require at least 8 characters');
expect(passwordValidator, /max_length:\s*128/, 'Backend standard password validator must cap passwords at 128 characters');
expect(passwordValidator, /require_uppercase:\s*true/, 'Backend standard password validator must require uppercase letters');
expect(passwordValidator, /require_lowercase:\s*true/, 'Backend standard password validator must require lowercase letters');
expect(passwordValidator, /require_digit:\s*true/, 'Backend standard password validator must require digits');
expect(passwordValidator, /require_special:\s*true/, 'Backend standard password validator must require special characters');

expect(resetPage, /function\s+validatePassword\s*\(/, 'Reset page must validate password policy before calling the API');
expect(resetPage, /value\.length\s*<\s*8/, 'Reset page must enforce the backend minimum password length');
expect(resetPage, /value\.length\s*>\s*128/, 'Reset page must enforce the backend maximum password length');
expect(resetPage, /\\s/, 'Reset page must reject whitespace before submitting');
expect(resetPage, /\[A-Z\]/, 'Reset page must require an uppercase letter before submitting');
expect(resetPage, /\[a-z\]/, 'Reset page must require a lowercase letter before submitting');
expect(resetPage, /\[0-9\]/, 'Reset page must require a digit before submitting');
expect(resetPage, /specialChars\.test\(value\)/, 'Reset page must require a special character before submitting');
expect(resetPage, /auth\.resetPassword\(token,\s*password\)/, 'Reset page must still call the reset-password API after validation');
expect(resetPage, /import\s*\{\s*fetchUser\s*\}\s*from\s*['"]\$lib\/stores\/auth\.svelte['"]/, 'Reset page must import the auth-store profile refresh helper');
expect(resetPage, /setToken\(res\.token\)[\s\S]*await\s+fetchUser\(\)/, 'Reset page must refresh the auth store after storing the reset JWT');
expect(loginPage, /href="\/forgot-password"/, 'Login page must link to the password reset request page');
expect(forgotPage, /auth\.forgotPassword\(/, 'Forgot password page must call the forgot-password API');
expect(forgotPage, /type="email"/, 'Forgot password page must collect an email address');
expect(forgotPage, /href="\/login"/, 'Forgot password page must link back to login');

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Password reset frontend/backend contract ok');
