<?php

declare(strict_types=1);

namespace App;

final class Clean
{
    public function add(int $a, int $b): int
    {
        return $a + $b;
    }

    public function greet(string $name): string
    {
        return 'Hello, ' . $name;
    }
}
