<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — end-to-end test of `scripts/pack-tarball-php.sh`.
 *
 * Asserts the bash pipeline produces a tarball whose name +
 * layout + sha256 sidecar match the convention 31.1 consumes.
 *
 * Synthetic vendor: a tempdir with a minimal `vendor/autoload.php`
 * + a stub `vendor/nexo/plugin-sdk/src/PluginAdapter.php`
 * substitutes for a real `composer install` run. SKIP_COMPOSER=1
 * env override bypasses the Composer step.
 */

const PLUGIN_ID = 'template_plugin_php';
const PLUGIN_VERSION = '0.1.0';

function fail(string $msg): never
{
    fwrite(STDERR, "FAIL: $msg\n");
    exit(1);
}

$templateRoot = dirname(__DIR__);

function rcopy(string $src, string $dst): void
{
    if (is_file($src)) {
        @mkdir(dirname($dst), 0755, true);
        if (!copy($src, $dst)) {
            fail("copy $src → $dst failed");
        }
        return;
    }
    if (!is_dir($src)) {
        return;
    }
    @mkdir($dst, 0755, true);
    foreach (scandir($src) as $entry) {
        if ($entry === '.' || $entry === '..') {
            continue;
        }
        rcopy("$src/$entry", "$dst/$entry");
    }
}

$work = sys_get_temp_dir() . '/pack-php-' . bin2hex(random_bytes(6));
$sdk  = sys_get_temp_dir() . '/sdk-stub-' . bin2hex(random_bytes(6));
$extr = sys_get_temp_dir() . '/pack-php-extract-' . bin2hex(random_bytes(6));
mkdir($work, 0755, true);
mkdir($sdk, 0755, true);
mkdir($extr, 0755, true);

// 1. Synthetic SDK stub.
mkdir("$sdk/src", 0755, true);
file_put_contents("$sdk/src/PluginAdapter.php", "<?php // stub SDK\n");
file_put_contents(
    "$sdk/composer.json",
    json_encode([
        'name' => 'nexo/plugin-sdk',
        'type' => 'library',
        'autoload' => ['psr-4' => ['Nexo\\Plugin\\Sdk\\' => 'src/']],
    ], JSON_PRETTY_PRINT),
);

// 2. Copy template fixture into work dir (manifest + scripts + src).
copy("$templateRoot/nexo-plugin.toml", "$work/nexo-plugin.toml");
rcopy("$templateRoot/src", "$work/src");
rcopy("$templateRoot/scripts", "$work/scripts");

// 3. Provide a synthetic vendor/ tree so SKIP_COMPOSER=1 has
//    something to pack. Mirrors what `composer install --no-dev
//    --optimize-autoloader --classmap-authoritative` would
//    produce.
mkdir("$work/vendor/composer", 0755, true);
file_put_contents("$work/vendor/autoload.php", "<?php // stub autoloader\n");
file_put_contents("$work/vendor/composer/autoload_classmap.php", "<?php return [];\n");
mkdir("$work/vendor/nexo/plugin-sdk/src", 0755, true);
file_put_contents("$work/vendor/nexo/plugin-sdk/src/PluginAdapter.php", "<?php // vendored SDK\n");

// 4. Run pack with SDK_SRC + SKIP_COMPOSER overrides.
$env = array_merge($_ENV, [
    'SDK_SRC' => $sdk,
    'SKIP_COMPOSER' => '1',
    'PATH' => getenv('PATH'),
]);
$cwd = $work;
$proc = proc_open(
    ['bash', 'scripts/pack-tarball-php.sh'],
    [0 => ['pipe', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']],
    $pipes,
    $cwd,
    $env,
);
if (!is_resource($proc)) {
    fail('proc_open pack-tarball failed');
}
fclose($pipes[0]);
$stdout = stream_get_contents($pipes[1]);
$stderr = stream_get_contents($pipes[2]);
fclose($pipes[1]);
fclose($pipes[2]);
$code = proc_close($proc);
if ($code !== 0) {
    fail("pack failed: code=$code stdout=$stdout stderr=$stderr");
}

// 5. Asset present.
$assetName = sprintf('%s-%s-noarch.tar.gz', PLUGIN_ID, PLUGIN_VERSION);
$asset = "$work/dist/$assetName";
$sidecar = "$work/dist/$assetName.sha256";
if (!is_file($asset)) {
    fail("asset missing: $asset");
}
if (!is_file($sidecar)) {
    fail("sha sidecar missing: $sidecar");
}

// 6. Sidecar is 64 lowercase hex chars.
$sidecarHex = trim(file_get_contents($sidecar));
if (strlen($sidecarHex) !== 64 || preg_match('/^[0-9a-f]{64}$/', $sidecarHex) !== 1) {
    fail("sidecar must be 64 lowercase hex chars, got: $sidecarHex");
}

// 7. Recompute sha256.
$computed = hash_file('sha256', $asset);
if ($computed !== $sidecarHex) {
    fail("sha256 mismatch: computed=$computed sidecar=$sidecarHex");
}

// 8. Re-extract via system tar + verify layout.
exec("tar -xzf " . escapeshellarg($asset) . " -C " . escapeshellarg($extr), $_, $tarCode);
if ($tarCode !== 0) {
    fail("tar -xzf failed code=$tarCode");
}
$top = array_diff(scandir($extr), ['.', '..']);
sort($top);
$expected = ['bin', 'lib', 'nexo-plugin.toml'];
sort($expected);
if (array_values($top) !== $expected) {
    fail('unexpected top-level entries: ' . implode(',', $top));
}
if (!is_file("$extr/bin/" . PLUGIN_ID)) {
    fail('launcher missing: bin/' . PLUGIN_ID);
}
if (!is_file("$extr/lib/plugin/main.php")) {
    fail('main.php missing in lib/plugin/');
}
if (!is_file("$extr/lib/vendor/autoload.php")) {
    fail('vendor/autoload.php missing in lib/');
}
if (!is_file("$extr/lib/vendor/nexo/plugin-sdk/src/PluginAdapter.php")) {
    fail('vendored SDK missing in lib/vendor/nexo/plugin-sdk/');
}

// 9. Launcher executable bit.
$mode = fileperms("$extr/bin/" . PLUGIN_ID) & 0o777;
if ($mode !== 0o755) {
    fail(sprintf('launcher mode should be 0o755, got %o', $mode));
}

fwrite(STDOUT, "ok 1 - pack_tarball_produces_canonical_layout\n");
exit(0);
