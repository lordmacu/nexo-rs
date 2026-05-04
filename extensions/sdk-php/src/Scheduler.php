<?php

declare(strict_types=1);

/**
 * Phase 31.5.c — Fiber-based cooperative scheduler.
 *
 * PHP's standard model is single-threaded blocking. To preserve
 * the contract invariant proven necessary by the TS + Python
 * SDKs ("the reader must NOT block on a slow handler"), we run
 * each broker.event handler inside a Fiber and let the dispatch
 * loop tick the scheduler between stdin polls.
 *
 * Plugin authors who await long-running operations should call
 * `Fiber::suspend()` at await points; otherwise their code runs
 * to completion in one scheduler tick.
 *
 * Mirrors `extensions/sdk-python/nexo_plugin_sdk/adapter.py`'s
 * `_drain_inflight` semantics and
 * `extensions/sdk-typescript/src/adapter.ts`'s `Set<Promise>`
 * pattern.
 */

namespace Nexo\Plugin\Sdk;

final class Scheduler
{
    /** @var \Fiber[] */
    private array $inflight = [];

    /**
     * Spawn a new Fiber running `$fn`. The Fiber starts
     * synchronously; if it suspends, it joins `$inflight` until
     * resumed or terminated.
     */
    public function spawn(callable $fn): \Fiber
    {
        $fiber = new \Fiber($fn);
        try {
            $fiber->start();
        } catch (\Throwable $e) {
            fwrite(STDERR, 'plugin: fiber spawn raised: ' . $e->getMessage() . "\n");
            return $fiber;
        }
        if (!$fiber->isTerminated()) {
            $this->inflight[] = $fiber;
        }
        return $fiber;
    }

    /**
     * Resume any suspended Fibers a single time. Drops Fibers
     * that have terminated. Call once per dispatch loop tick.
     */
    public function tick(): void
    {
        $next = [];
        foreach ($this->inflight as $fiber) {
            if ($fiber->isTerminated()) {
                continue;
            }
            if ($fiber->isSuspended()) {
                try {
                    $fiber->resume();
                } catch (\Throwable $e) {
                    fwrite(STDERR, 'plugin: fiber resume raised: ' . $e->getMessage() . "\n");
                    continue;
                }
            }
            if (!$fiber->isTerminated()) {
                $next[] = $fiber;
            }
        }
        $this->inflight = $next;
    }

    /**
     * Loop tick + 1ms sleep until inflight empty. Bounded by
     * the host supervisor's grace period in production.
     */
    public function drain(): void
    {
        while ($this->inflight !== []) {
            $this->tick();
            if ($this->inflight === []) {
                break;
            }
            usleep(1_000);
        }
    }

    public function size(): int
    {
        return count($this->inflight);
    }
}
