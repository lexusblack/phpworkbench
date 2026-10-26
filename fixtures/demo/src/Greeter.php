<?php

declare(strict_types=1);

namespace Demo;

final class Greeter
{
    public function __construct(private readonly string $name = 'World')
    {
    }

    public function greet(): string
    {
        return "Hello, {$this->name}!";
    }
}
