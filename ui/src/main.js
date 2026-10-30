import { mount } from 'svelte';
import '@material/web/button/filled-button.js';
import App from './App.svelte';
import './style.css';

mount(App, { target: document.getElementById('app') });
