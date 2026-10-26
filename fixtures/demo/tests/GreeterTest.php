<?php

declare(strict_types=1);

namespace Demo\Tests;

use Demo\Greeter;
use PHPUnit\Framework\TestCase;

final class GreeterTest extends TestCase
{
    public function testGreetsByName(): void
    {
        $this->assertSame('Hello, PHP!', (new Greeter('PHP'))->greet());
    }

    public function testDefaultsToWorld(): void
    {
        $this->assertSame('Hello, World!', (new Greeter())->greet());
    }
}
