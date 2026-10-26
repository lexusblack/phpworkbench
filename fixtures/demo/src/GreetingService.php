<?php

declare(strict_types=1);

namespace Demo;

final class GreetingService
{
    /** @var list<Greeter> */
    private array $greeters = [];

    public function add(Greeter $greeter): void
    {
        $this->greeters[] = $greeter;
    }

    /** @return list<string> */
    public function greetAll(): array
    {
        return array_map(
            fn(Greeter $greeter): string => $greeter->greet(),
            $this->greeters,
        );
    }
}
