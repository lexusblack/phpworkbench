<?php

declare(strict_types=1);

namespace App\Tests;

use App\Calculator;
use PHPUnit\Framework\TestCase;

final class CalculatorTest extends TestCase
{
    public function testAddsNumbers(): void
    {
        self::assertSame(5, (new Calculator())->add(2, 3));
    }

    public function testSubtractsNumbers(): void
    {
        self::assertSame(2, (new Calculator())->subtract(5, 3));
    }

    public function testMultipliesNumbers(): void
    {
        self::assertSame(12, (new Calculator())->multiply(3, 4));
    }
}
