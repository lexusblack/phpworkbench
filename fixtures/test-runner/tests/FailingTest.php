<?php

declare(strict_types=1);

namespace App\Tests;

use PHPUnit\Framework\TestCase;

final class FailingTest extends TestCase
{
    public function testFailsOnPurpose(): void
    {
        self::assertSame(4, 2 + 3);
    }

    public function testIsSkipped(): void
    {
        self::markTestSkipped('skipped for fixture');
    }
}
