<?php

declare(strict_types=1);

require __DIR__ . '/src/Greeter.php';
require __DIR__ . '/src/GreetingService.php';

use Demo\Greeter;
use Demo\GreetingService;

$service = new GreetingService();
$service->add(new Greeter('PHP'));

echo implode("\n", $service->greetAll()), "\n";
