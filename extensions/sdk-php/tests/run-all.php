<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — stdlib-style test runner. Mirrors the TS
 * SDK's `node:test` choice and Python's `unittest` choice — no
 * PHPUnit / Pest dep.
 *
 * Each `test_*.php` exits 0 on success, non-zero with FAIL line
 * on stderr on failure. We spawn each, collect exit codes, and
 * print a summary.
 */

$dir = __DIR__;
$files = [];
foreach (scandir($dir) as $entry) {
    if (preg_match('/^test_.*\.php$/', $entry) === 1) {
        $files[] = $dir . '/' . $entry;
    }
}
sort($files);

$pass = 0;
$fail = 0;
$failures = [];
foreach ($files as $file) {
    $relName = basename($file);
    fwrite(STDOUT, "# $relName\n");
    $proc = proc_open(
        ['php', $file],
        [
            0 => ['pipe', 'r'],
            1 => ['pipe', 'w'],
            2 => ['pipe', 'w'],
        ],
        $pipes,
    );
    if (!is_resource($proc)) {
        $fail++;
        $failures[] = "$relName: proc_open failed";
        continue;
    }
    fclose($pipes[0]);
    $stdout = stream_get_contents($pipes[1]);
    $stderr = stream_get_contents($pipes[2]);
    fclose($pipes[1]);
    fclose($pipes[2]);
    $code = proc_close($proc);
    fwrite(STDOUT, $stdout);
    if ($code !== 0) {
        $fail++;
        $failures[] = "$relName: exit $code\n" . $stderr;
        fwrite(STDERR, "FAIL $relName ($code): $stderr\n");
    } else {
        $pass++;
    }
}

fwrite(STDOUT, "\n# tests passed: $pass\n");
fwrite(STDOUT, "# tests failed: $fail\n");
exit($fail === 0 ? 0 : 1);
